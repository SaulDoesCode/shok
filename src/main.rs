use mimalloc::MiMalloc;
#[global_allocator] static GLOBAL: MiMalloc = MiMalloc;

use anyhow::anyhow;
use base64::Engine as _;
use lazy_static::lazy_static;
use salvo::{conn::rustls::{Keycert, RustlsConfig}, http::{*}, prelude::*, rate_limiter::*, logging::Logger, sse::{SseKeepAlive, SseEvent}};
use serde::{Serialize, Deserialize};
use serde_json::json;
use sthash::Hasher;
use std::{io::{Read, Write}, fs::File, path::{Path, PathBuf}, marker::PhantomData, sync::Arc, time::Duration, mem::forget, collections::HashMap};
use rand::{thread_rng, distributions::Alphanumeric, Rng};
use redb::{Database, ReadableTable, TableDefinition, MultimapTableDefinition, ReadableMultimapTable, WriteTransaction};
use tantivy::{
    doc,
    collector::TopDocs,
    query::{QueryParser, TermQuery},
    schema::*,
    Index, IndexWriter, DateTime//, IndexReader, ReloadPolicy,
};
use futures_util::StreamExt;
use tokio::sync::mpsc;
use tokio_stream::wrappers::UnboundedReceiverStream;
use parking_lot::RwLock;

type U8s = Vec<u8>;
type Strings = Vec<String>;

#[derive(Debug)]
enum Message {
    UserId(usize),
    Reply(String)
}

type Accounts = dashmap::DashMap<usize, mpsc::UnboundedSender<Message>>;
lazy_static!{
    static ref IA: RwLock<Vec<PathBuf>> = RwLock::new(read_all_file_names_in_dir("./uploaded/ImageAssets/").expect("could not read image assets directory's paths all the way through.. too heavy perhaps, perhaps it is not there anymore"));
    static ref SINCE_LAST_STATIC_CHECK: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    static ref STATIC_DIR_PATHS: parking_lot::RwLock<Vec<PathBuf>> = parking_lot::RwLock::new(read_all_file_names_in_dir(STATIC_DIR).expect("could not walk the static dir for some reason"));
    static ref DB: Database = Database::create("./shok.db").expect("Oh no! Could not open the specified database. (./shok.db) :(");
    static ref TOKEN_HASHER: sthash::Hasher = {
        let key = PWD.0.as_slice();
        sthash::Hasher::new(sthash::Key::from_seed(key, Some(b"shok-tk")), Some(b"tokens"))
    };
    static ref RESOURCE_HASHER: sthash::Hasher = {
        let key = PWD.0.as_slice();
        sthash::Hasher::new(sthash::Key::from_seed(key, Some(b"resources")), Some(b"insurance"))
    };
    static ref SEARCH: Search = Search::build_512mb().expect("Failed to build search.");
    static ref SINCE_START: u64 = now();
    static ref ONLINE_ACCOUNTS: Accounts = Accounts::new();
    static ref B64: base64::engine::GeneralPurpose = {
        let abc = base64::alphabet::Alphabet::new("+_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789").expect("aplhabet was too much for base64, sorry");
        base64::engine::GeneralPurpose::new(&abc, base64::engine::general_purpose::GeneralPurposeConfig::new().with_encode_padding(false).with_decode_allow_trailing_bits(true))
    };
    static ref PWD: (U8s, Hasher) = get_or_generate_admin_password();
}
const ADMIN_PWD_FILE_PATH: &str = "./secrets/ADMIN_PWD.txt";
const SEARCH_INDEX_PATH: &str = "./search-index";

//                                   id, writ.ts
const LIKES: MultimapTableDefinition<u64, u64> = MultimapTableDefinition::new("likes");
//                                   writ.ts, id
const LIKED_BY: MultimapTableDefinition<u64, u64> = MultimapTableDefinition::new("liked_by");

fn like_writ(id: u64, like_id: u64) -> anyhow::Result<()> {
    let wtx = DB.begin_write()?;
    {
        let mut t = wtx.open_multimap_table(LIKES)?;
        let was_liked = t.insert(id, like_id)?;
        if was_liked {
            t.remove(id, like_id)?;
            let mut t = wtx.open_multimap_table(LIKED_BY)?;
            t.remove(like_id, id)?;
        } else {
            let mut t = wtx.open_multimap_table(LIKED_BY)?;
            t.insert(like_id, id)?;
        }
    }
    wtx.commit()?;
    Ok(())
}

fn unlike_writ(id: u64, like_id: u64) -> anyhow::Result<()> {
    let wtx = DB.begin_write()?;
    {
        let mut t = wtx.open_multimap_table(LIKES)?;
        let was_liked = t.remove(id, like_id)?;
        if was_liked {
            let mut t = wtx.open_multimap_table(LIKED_BY)?;
            t.remove(like_id, id)?;
        } else {
            t.insert(id, like_id)?;
            let mut t = wtx.open_multimap_table(LIKED_BY)?;
            t.insert(like_id, id)?;
        }
    }
    wtx.commit()?;
    Ok(())
}

fn remove_all_likes_from_account(wtx: &WriteTransaction, id: u64) -> anyhow::Result<()> {
    let mut t = wtx.open_multimap_table(LIKES)?;
    let mut mmv = t.remove_all(id)?;
    let mut tlb = wtx.open_multimap_table(LIKED_BY)?;
    while let Some(r) = mmv.next() {
        tlb.remove(r?.value(), id)?;
    }
    Ok(())
}

fn get_writ_likes(id: u64, limit: usize) -> anyhow::Result<Vec<u64>> {
    let rtx = DB.begin_read()?;
    let t = rtx.open_multimap_table(LIKED_BY)?;
    let mut mmv = t.get(id)?;
    let mut likes = Vec::new();
    while let Some(r) = mmv.next() {
        likes.push(r?.value());
        if likes.len() >= limit {
            break;
        }
    }
    Ok(likes)
}

/*#[allow(dead_code)]
fn writs_likes(writs: &[u64]) -> anyhow::Result<HashMap<u64, Vec<u64>>> {
    let rtx = DB.begin_read()?;
    let t = rtx.open_multimap_table(LIKED_BY)?;
    let mut likes = HashMap::new();
    for id in writs {
        let mut mmv = t.get(*id)?;
        let mut likes_vec = Vec::new();
        while let Some(r) = mmv.next() {
            likes_vec.push(r?.value());
        }
        likes.insert(*id, likes_vec);
    }
    Ok(likes)
}*/

fn get_liked_by(id: u64, limit: usize) -> anyhow::Result<Vec<u64>> {
    let rtx = DB.begin_read()?;
    let t = rtx.open_multimap_table(LIKES)?;
    let mut mmv = t.get(id)?;
    let mut likes = Vec::new();
    while let Some(r) = mmv.next() {
        likes.push(r?.value());
        if likes.len() >= limit {
            break;
        }
    }
    Ok(likes)
}

fn is_liked_by(id: u64, like_id: u64) -> anyhow::Result<bool> {
    let rtx = DB.begin_read()?;
    let t = rtx.open_multimap_table(LIKED_BY)?;
    let mut mmv = t.get(like_id)?;
    while let Some(r) = mmv.next() {
        if r?.value() == id {
            return Ok(true);
        }
    }
    Ok(false)
}

const FOLLOWS: MultimapTableDefinition<u64, u64> = MultimapTableDefinition::new("follows");
const FOLLOWED_BY: MultimapTableDefinition<u64, u64> = MultimapTableDefinition::new("followed_by");

fn follow_account(id: u64, follow_id: u64) -> anyhow::Result<()> {
    let wtx = DB.begin_write()?;
    {
        let mut t = wtx.open_multimap_table(FOLLOWS)?;
        t.insert(id, follow_id)?;
        let mut t = wtx.open_multimap_table(FOLLOWED_BY)?;
        t.insert(follow_id, id)?;
    }
    wtx.commit()?;
    Ok(())
}

fn unfollow_account(id: u64, follow_id: u64) -> anyhow::Result<()> {
    let wtx = DB.begin_write()?;
    {
        let mut t = wtx.open_multimap_table(FOLLOWS)?;
        t.remove(id, follow_id)?;
        let mut t = wtx.open_multimap_table(FOLLOWED_BY)?;
        t.remove(follow_id, id)?;
    }
    wtx.commit()?;
    Ok(())
}
/*#[allow(dead_code)]
fn follow_account_internal(wtx: &WriteTransaction, id: u64, follow_id: u64) -> anyhow::Result<()> {
    let mut t = wtx.open_multimap_table(FOLLOWS)?;
    t.insert(id, follow_id)?;
    let mut t = wtx.open_multimap_table(FOLLOWED_BY)?;
    t.insert(follow_id, id)?;
    Ok(())
}*/

fn unfollow_account_internal(wtx: &WriteTransaction, id: u64, follow_id: u64) -> anyhow::Result<()> {
    let mut t = wtx.open_multimap_table(FOLLOWS)?;
    t.remove(id, follow_id)?;
    let mut t = wtx.open_multimap_table(FOLLOWED_BY)?;
    t.remove(follow_id, id)?;
    Ok(())
}

fn get_followers(id: u64, limit: usize) -> anyhow::Result<Vec<u64>> {
    let rtx = DB.begin_read()?;
    let t = rtx.open_multimap_table(FOLLOWED_BY)?;
    let mut mmv = t.get(id)?;
    let mut followers = Vec::new();
    while let Some(r) = mmv.next() {
        followers.push(r?.value());
        if followers.len() >= limit { break; }
    }
    Ok(followers)
}

fn get_following(id: u64, limit: usize) -> anyhow::Result<Vec<u64>> {
    let rtx = DB.begin_read()?;
    let t = rtx.open_multimap_table(FOLLOWS)?;
    let mut mmv = t.get(id)?;
    let mut following = Vec::new();
    while let Some(r) = mmv.next() {
        following.push(r?.value());
        if following.len() >= limit { break; }
    }
    Ok(following)
}

fn is_followed_by(id: u64, follow_id: u64) -> anyhow::Result<bool> {
    let rtx = DB.begin_read()?;
    let t = rtx.open_multimap_table(FOLLOWED_BY)?;
    let mut mmv = t.get(id)?;
    while let Some(r) = mmv.next() {
        if r?.value() == follow_id { return Ok(true); }
    }
    Ok(false)
}

fn is_following(id: u64, follow_id: u64) -> anyhow::Result<bool> {
    let rtx = DB.begin_read()?;
    let t = rtx.open_multimap_table(FOLLOWS)?;
    let mut mmv = t.get(id)?;
    while let Some(r) = mmv.next() {
        if r?.value() == follow_id { return Ok(true); }
    }
    Ok(false)
}

const SV_CHANGE_WATCHERS: MultimapTableDefinition<u64, &str> = MultimapTableDefinition::new("sv_change_watchers");

fn watch_changes(id: u64, key: &str) -> anyhow::Result<()> {
    let wtx = DB.begin_write()?;
    {
        let mut t = wtx.open_multimap_table(SV_CHANGE_WATCHERS)?;
        t.insert(id, key)?;
    }
    wtx.commit()?;
    Ok(())
}

fn unwatch_changes(id: u64, key: &str) -> anyhow::Result<()> {
    let wtx = DB.begin_write()?;
    {
        let mut wt = wtx.open_multimap_table(SV_CHANGE_WATCHERS)?;
        wt.remove(id, key)?;
    }
    wtx.commit()?;
    Ok(())
}

fn internal_notify_changes(t: &redb::MultimapTable<u64, &str>, id: u64, key: &str, inserted: bool) -> anyhow::Result<()> {
    let mut mmv = t.get(id)?;
    while let Some(r) = mmv.next() {
        if r?.value() != key { continue; }
        let op = if inserted { "inserted" } else { "removed" };
        auto_msg(format!("{key}: {op}")).i(id);
    }
    Ok(())
}

#[handler]
async fn chat_send(req: &mut Request, res: &mut Response) {
    let uid = match session_check(req, None).await {
        Some(id) => id,
        None => {
            uares(res, "not logged in");
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
                    if let Ok(acc) = Account::from_moniker(moniker) {
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
                                if moniker == "me" || moniker == "I" {
                                    auto_msg(format!("{} is {}", moniker, uid)).i(uid as u64);
                                } else if let Ok(acc) = Account::from_moniker(moniker) {
                                    auto_msg(format!("{} is {}", moniker, acc.id)).i(uid as u64);
                                } else {
                                    str_auto_msg("No such identity found").i(uid as u64);
                                }
                            } else {
                                auto_msg(format!("missing moniker")).i(uid as u64);
                            }
                        }
                        "whoami" => {
                            auto_msg(format!("you are {}", uid)).i(uid as u64);
                        }
                        "restart" => {
                            if uid == ADMIN_ID as usize {
                                auto_msg(format!("restarting server")).i(uid as u64);
                                restart_server();
                            } else {
                                auto_msg(format!("not admin")).i(uid as u64);
                            }
                        }
                        "follow" => {
                            if let Some(moniker) = args.next() {
                                if let Ok(acc) = Account::from_moniker(moniker) {
                                    if let Err(e) = follow_account(uid as u64, acc.id) {
                                        auto_msg(format!("failed to follow {}: {}", moniker, e)).i(uid as u64);
                                    } else {
                                        auto_msg(format!("followed {}", moniker)).i(uid as u64);
                                    }
                                } else {
                                    str_auto_msg("No such identity found, failed to parse id as number, tried as a moniker, both failed, cannot follow").i(uid as u64);
                                }
                            } else {
                                auto_msg(format!("missing moniker")).i(uid as u64);
                            }
                        }
                        "unfollow" => {
                            if let Some(moniker) = args.next() {
                                if let Ok(acc) = Account::from_moniker(moniker) {
                                    if let Err(e) = unfollow_account(uid as u64, acc.id) {
                                        auto_msg(format!("failed to unfollow {}: {}", moniker, e)).i(uid as u64);
                                    } else {
                                        auto_msg(format!("unfollowed {}", moniker)).i(uid as u64);
                                    }
                                } else {
                                    str_auto_msg("No such identity found, failed to parse id as number, tried as a moniker, both failed, cannot unfollow").i(uid as u64);
                                }
                            } else {
                                auto_msg(format!("missing moniker")).i(uid as u64);
                            }
                        }
                        "transfer" => {
                            if let Some(to) = args.next() {
                                if let Ok(acc) = Account::from_moniker(to) {
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
                            // if there is a next arg that can be read as a u64 ammount then if the account is admin add balance
                            if let Some(amount) = args.next() {
                                if let Ok(amount) = amount.parse::<u64>() {
                                    if let Ok(mut acc) = Account::from_id(uid as u64) {
                                        if acc.id == ADMIN_ID {
                                            auto_msg(format!("adding {} to balance", amount)).i(uid as u64);
                                            acc.balance += amount;
                                            match acc.save(false) {
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
                            if let Ok(acc) = Account::from_id(uid as u64) {
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
                        "server-timestamp" => {
                            auto_msg(format!("server time: {}", now())).i(uid as u64);
                        }
                        "cmd" => {
                            if let Ok(acc) = Account::from_id(uid as u64) {
                                if acc.id == ADMIN_ID {
                                    if let Some(cmd) = args.next() {
                                        let args = args.collect::<Vec<&str>>();
                                        auto_msg(format!("running cmd: {}", cmd)).i(uid as u64);
                                        // run the command on the machine and return the output
                                        match std::process::Command::new(cmd).args(args).output() {
                                            Ok(output) => {
                                                auto_msg(format!("output: {}", String::from_utf8_lossy(&output.stdout))).i(uid as u64);
                                            }
                                            Err(e) => {
                                                auto_msg(format!("failed to run cmd: {}", e)).i(uid as u64);
                                            }
                                        }
                                    } else {
                                        auto_msg(format!("missing cmd")).i(uid as u64);
                                    }
                                } else {
                                    auto_msg(format!("not admin")).i(uid as u64);
                                }
                            } else {
                                auto_msg(format!("failed to get account")).i(uid as u64);
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
                                    if let Ok(acc) = Account::from_moniker(to) {
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
                        "set" => {
                            if let Some(key) = args.next() {
                                let mut msg = String::new();
                                while let Some(arg) = args.next() {
                                    msg.push_str(arg);
                                    msg.push(' ');
                                }
                                if msg == "" {
                                    auto_msg(format!("missing message")).i(uid as u64);
                                } else if  let Ok(val) = serde_json::from_str::<serde_json::Value>(&msg) {
                                    if let Ok(svs) = ScopedVariableStore::<serde_json::Value>::open(uid as u64) {
                                        match svs.set(key, val) {
                                            Ok(_) => {
                                                auto_msg(format!("set {} to {}", key, msg)).i(uid as u64);
                                            },
                                            Err(e) => {
                                                auto_msg(format!("failed to set {} to {}, error: {}", key, msg, e.to_string())).i(uid as u64);
                                            }
                                        }
                                    } else {
                                        auto_msg(format!("failed to open scoped variable store")).i(uid as u64);
                                    }
                                }
                            }
                        }
                        "get" => {
                            if let Some(key) = args.next() {
                                if let Ok(svs) = ScopedVariableStore::<serde_json::Value>::open(uid as u64) {
                                    if let Ok(val) = svs.get(key) {
                                        auto_msg(format!("{}: {}", key, serde_json::to_string(&val).unwrap_or_else(|_| "failed to serialize value".to_string()))).i(uid as u64);
                                    } else {
                                        auto_msg(format!("failed to get {}", key)).i(uid as u64);
                                    }
                                } else {
                                    auto_msg(format!("failed to open scoped variable store")).i(uid as u64);
                                }
                            }
                        }
                        "rm" => {
                            if let Some(key) = args.next() {
                                if let Ok(svs) = ScopedVariableStore::<serde_json::Value>::open(uid as u64) {
                                    if let Ok(val) = svs.rm(key) {
                                        auto_msg(format!("removed {}: {}", key, serde_json::to_string(&val).unwrap_or_else(|_| "failed to serialize value".to_string()))).i(uid as u64);
                                    } else {
                                        auto_msg(format!("failed to remove {}", key)).i(uid as u64);
                                    }
                                } else {
                                    auto_msg(format!("failed to open scoped variable store")).i(uid as u64);
                                }
                            }
                        }
                        "watch" => {
                            if let Some(key) = args.next() {
                                if let Err(e) = watch_changes(uid as u64, key) {
                                    auto_msg(format!("failed to add a watcher for {}; error: {}", key, e)).i(uid as u64);
                                }
                            }
                        }
                        "unwatch" => {
                            if let Some(key) = args.next() {
                                if let Err(e) = unwatch_changes(uid as u64, key) {
                                    auto_msg(format!("failed to remove a watcher for {}; error: {}", key, e)).i(uid as u64);
                                }
                            }
                        }
                        "collection" | "cl" => if let Some(op) = args.next() {
                            match op {
                                "add" => {
                                    // make a new svs collection if there isn't one, the next arg will be the moniker of the collection, the one after that is the moniker of the variable, if there's a value after that set the variable to that value
                                    // use svs.add_vars_to_collection
                                    if let Some(moniker) = args.next() {
                                        if let Ok(svs) = ScopedVariableStore::<serde_json::Value>::open(uid as u64) {
                                            if let Ok(val) = svs.add_vars_to_collection(moniker, args.collect::<Vec<&str>>().as_slice()) {
                                                auto_msg(format!("added to collection {}: {}", moniker, serde_json::to_string(&val).unwrap_or_else(|_| "failed to serialize value".to_string()))).i(uid as u64);
                                            } else {
                                                auto_msg(format!("failed to add to collection {}", moniker)).i(uid as u64);
                                            }
                                        } else {
                                            auto_msg(format!("failed to open scoped variable store")).i(uid as u64);
                                        }
                                    }
                                }
                                "rm" => {
                                    // remove a variable from a collection
                                    // use svs.rm_var_from_collection
                                    if let Some(moniker) = args.next() {
                                        // if there are no more args, just remove the collection using svs.rm_collection
                                        // use svs.rm_collection
                                        let first_var = args.next();
                                        if first_var.is_none() {
                                            if let Ok(svs) = ScopedVariableStore::<serde_json::Value>::open(uid as u64) {
                                                if let Ok(val) = svs.rm_collection(moniker) {
                                                    auto_msg(format!("removed collection {}: {}", moniker, serde_json::to_string(&val).unwrap_or_else(|_| "failed to serialize value".to_string()))).i(uid as u64);
                                                } else {
                                                    auto_msg(format!("failed to remove collection {}", moniker)).i(uid as u64);
                                                }
                                            } else {
                                                auto_msg(format!("failed to open scoped variable store")).i(uid as u64);
                                            }
                                        } else {
                                            if let Ok(svs) = ScopedVariableStore::<serde_json::Value>::open(uid as u64) {
                                                let mut vars = args.collect::<Vec<&str>>();
                                                // add the first_var back in
                                                vars.insert(0, first_var.unwrap());
                                                if let Err(e) = svs.rm_vars_from_collection(moniker, vars.as_slice()) {
                                                    auto_msg(format!("failed to remove vars from collection {}, error: {}", moniker, e.to_string())).i(uid as u64);
                                                } else {
                                                    auto_msg(format!("removed vars from collection {}: {}", moniker, vars.join(", "))).i(uid as u64);
                                                }
                                            } else {
                                                auto_msg(format!("failed to open scoped variable store")).i(uid as u64);
                                            }
                                        }
                                    }
                                }
                                "get" => {
                                    if let Some(moniker) = args.next() {
                                        if let Ok(svs) = ScopedVariableStore::<serde_json::Value>::open(uid as u64) {
                                            if let Ok(val) = svs.get_collection(moniker) {
                                                auto_msg(format!("collection {}: {}", moniker, serde_json::to_string(&val).unwrap_or_else(|_| "failed to serialize value".to_string()))).i(uid as u64);
                                            } else {
                                                auto_msg(format!("failed to get collection {}", moniker)).i(uid as u64);
                                            }
                                        } else {
                                            auto_msg(format!("failed to open scoped variable store")).i(uid as u64);
                                        }
                                    }
                                }
                                "has" => {
                                    if let Some(moniker) = args.next() {
                                        if let Ok(svs) = ScopedVariableStore::<serde_json::Value>::open(uid as u64) {
                                            let vars = args.collect::<Vec<&str>>();
                                            if let Ok(val) = svs.check_collection_membership(moniker, vars.as_slice()) {
                                                if val {
                                                    auto_msg(format!("collection {} has {}", moniker, vars.join(", "))).i(uid as u64);
                                                } else {
                                                    auto_msg(format!("collection {} does not have {}", moniker, vars.join(", "))).i(uid as u64);
                                                }
                                            } else {
                                                auto_msg(format!("failed to check collection membership {}", moniker)).i(uid as u64);
                                            }
                                        } else {
                                            auto_msg(format!("failed to open scoped variable store")).i(uid as u64);
                                        }
                                    }
                                }
                                _ => {
                                    if let Ok(svs) = ScopedVariableStore::<serde_json::Value>::open(uid as u64) {
                                        if let Ok(val) = svs.get_collection(op) {
                                            auto_msg(format!("collection {}: {}", op, serde_json::to_string(&val).unwrap_or_else(|_| "failed to serialize value".to_string()))).i(uid as u64);
                                        } else {
                                            auto_msg(format!("failed to get collection {}", op)).i(uid as u64);
                                        }
                                    } else {
                                        auto_msg(format!("failed to open scoped variable store")).i(uid as u64);
                                    }
                                }
                            }
                        }
                        "set-tags" => {
                            if let Some(moniker) = args.next() {
                                if let Ok(svs) = ScopedVariableStore::<serde_json::Value>::open(uid as u64) {
                                    if let Ok(val) = svs.set_tags(moniker, args.collect::<Vec<&str>>().as_slice()) {
                                        auto_msg(format!("set tags on {}: {}", moniker, serde_json::to_string(&val).unwrap_or_else(|_| "failed to serialize value".to_string()))).i(uid as u64);
                                    } else {
                                        auto_msg(format!("failed to set tags on {}", moniker)).i(uid as u64);
                                    }
                                } else {
                                    auto_msg(format!("failed to open scoped variable store")).i(uid as u64);
                                }
                            }
                        }
                        "get-tags" => {
                            if let Some(moniker) = args.next() {
                                if let Ok(svs) = ScopedVariableStore::<serde_json::Value>::open(uid as u64) {
                                    if let Ok(val) = svs.get_tags(moniker) {
                                        auto_msg(format!("tags on {}: {}", moniker, serde_json::to_string(&val).unwrap_or_else(|_| "failed to serialize value".to_string()))).i(uid as u64);
                                    } else {
                                        auto_msg(format!("failed to get tags on {}", moniker)).i(uid as u64);
                                    }
                                } else {
                                    auto_msg(format!("failed to open scoped variable store")).i(uid as u64);
                                }
                            }
                        }
                        "unset-tags" => {
                            if let Some(moniker) = args.next() {
                                if let Ok(svs) = ScopedVariableStore::<serde_json::Value>::open(uid as u64) {
                                    if let Ok(val) = svs.unset_tags(moniker, args.collect::<Vec<&str>>().as_slice()) {
                                        auto_msg(format!("unset tags on {}: {}", moniker, serde_json::to_string(&val).unwrap_or_else(|_| "failed to serialize value".to_string()))).i(uid as u64);
                                    } else {
                                        auto_msg(format!("failed to unset tags on {}", moniker)).i(uid as u64);
                                    }
                                } else {
                                    auto_msg(format!("failed to open scoped variable store")).i(uid as u64);
                                }
                            }
                        }
                        "vars-with-tags" => {
                            // the next args are tags, use svs.get_vars_with_tags to find them
                            let tags = args.collect::<Vec<&str>>();
                            if let Ok(svs) = ScopedVariableStore::<serde_json::Value>::open(uid as u64) {
                                if let Ok(val) = svs.get_vars_with_tags(tags.as_slice(), if uid as u64 == ADMIN_ID { 2048 } else { 256 }) {
                                    auto_msg(format!("vars with tags {}: {}", tags.join(", "), serde_json::to_string(&val).unwrap_or_else(|_| "failed to serialize value".to_string()))).i(uid as u64);
                                } else {
                                    auto_msg(format!("failed to get vars with tags {}", tags.join(", "))).i(uid as u64);
                                }
                            } else {
                                auto_msg(format!("failed to open scoped variable store")).i(uid as u64);
                            }
                        }
                        "exp" => {
                            if let Some(moniker) = args.next() {
                                if let Some(time) = args.next() {
                                    let time: u64 = if time.ends_with("m") {
                                        // minutes
                                        let time = time.trim_end_matches("m");
                                        if let Ok(time) = time.parse::<u64>() {
                                            now() + (time * 60)
                                        } else {
                                            auto_msg(format!("failed to parse time")).i(uid as u64);
                                            return;
                                        }
                                    } else if time.ends_with("s") {
                                        // seconds
                                        let time = time.trim_end_matches("s");
                                        if let Ok(time) = time.parse::<u64>() {
                                            now() + time
                                        } else {
                                            auto_msg(format!("failed to parse time")).i(uid as u64);
                                            return;
                                        }
                                    } else if time.ends_with("h") {
                                        // hours
                                        let time = time.trim_end_matches("h");
                                        if let Ok(time) = time.parse::<u64>() {
                                            now() + (time * 60 * 60)
                                        } else {
                                            auto_msg(format!("failed to parse time")).i(uid as u64);
                                            return;
                                        }
                                    } else if time.ends_with("d") {
                                        // days
                                        let time = time.trim_end_matches("d");
                                        if let Ok(time) = time.parse::<u64>() {
                                            now() + (time * 60 * 60 * 24)
                                        } else {
                                            auto_msg(format!("failed to parse time")).i(uid as u64);
                                            return;
                                        }
                                    } else if time.ends_with("w") {
                                        // weeks
                                        let time = time.trim_end_matches("w");
                                        if let Ok(time) = time.parse::<u64>() {
                                            now() + (time * 60 * 60 * 24 * 7)
                                        } else {
                                            auto_msg(format!("failed to parse time")).i(uid as u64);
                                            return;
                                        }
                                    } else if time.ends_with("y") {
                                        // years
                                        let time = time.trim_end_matches("y");
                                        if let Ok(time) = time.parse::<u64>() {
                                            now() + (time * 60 * 60 * 24 * 365)
                                        } else {
                                            auto_msg(format!("failed to parse time")).i(uid as u64);
                                            return;
                                        }
                                    } else if let Ok(time) = time.parse::<u64>() {
                                        time
                                    } else {
                                        auto_msg(format!("failed to parse time")).i(uid as u64);
                                        return;
                                    };
                                    if let Ok(svs) = ScopedVariableStore::<serde_json::Value>::open(uid as u64) {
                                        if let Ok(_) = svs.register_expiry(moniker, time) {
                                            auto_msg(format!("registered expiry {}", moniker)).i(uid as u64);
                                        } else {
                                            auto_msg(format!("failed to register expiry {}", moniker)).i(uid as u64);
                                        }
                                    } else {
                                        auto_msg(format!("failed to open scoped variable store")).i(uid as u64);
                                    }
                                } else {
                                    auto_msg(format!("missing time")).i(uid as u64);
                                }
                            } else {
                                auto_msg(format!("missing moniker")).i(uid as u64);
                            }
                        }
                        "perms" => {
                            // add, remove, modify, check
                            // get op
                            let op = match args.next() {
                                Some(op) => op,
                                None => {
                                    auto_msg(format!("missing op")).i(uid as u64);
                                    return;
                                }
                            };
                            // get the perm_schema id:u32
                            let pm: u32 = match args.next() {
                                Some(pm) => match pm.parse::<u32>() {
                                    Ok(pm) => pm,
                                    Err(e) => {
                                        auto_msg(format!("failed to parse perm_schema id: {}", e)).i(uid as u64);
                                        return;
                                    }
                                },
                                None => {
                                    auto_msg(format!("missing perm_schema id")).i(uid as u64);
                                    return;
                                }
                            };

                            match op {
                                "add" => {
                                    let perms = args.collect::<Vec<&str>>();
                                    match PermSchema::add_perms(pm, &perms) {
                                        Ok(_) => {
                                            auto_msg(format!("added perms to perm_schema {}", pm)).i(uid as u64);
                                        }
                                        Err(e) => {
                                            auto_msg(format!("failed to add perms to perm_schema {}: {}", pm, e)).i(uid as u64);
                                        }
                                    }
                                }
                                "rm" => {
                                    let perms = args.collect::<Vec<&str>>();
                                    match PermSchema::rm_perms(pm, &perms) {
                                        Ok(_) => {
                                            auto_msg(format!("removed perms from perm_schema {}", pm)).i(uid as u64);
                                        }
                                        Err(e) => {
                                            auto_msg(format!("failed to remove perms from perm_schema {}: {}", pm, e)).i(uid as u64);
                                        }
                                    }
                                }
                                "get" => match PermSchema::get_perms(pm) {
                                    Ok(perms) => {
                                        auto_msg(format!("perms on perm_schema {}: {}", pm, perms.join(", "))).i(uid as u64);
                                    }
                                    Err(e) => {
                                        auto_msg(format!("failed to get perms on perm_schema {}: {}", pm, e)).i(uid as u64);
                                    }
                                }
                                "check" => {
                                    let perms = args.collect::<Vec<&str>>();
                                    match PermSchema::check_for(pm, &perms) {
                                        Ok(val) => {
                                            if val {
                                                auto_msg(format!("perm_schema {} has perms {}", pm, perms.join(", "))).i(uid as u64);
                                            } else {
                                                auto_msg(format!("perm_schema {} does not have perms {}", pm, perms.join(", "))).i(uid as u64);
                                            }
                                        }
                                        Err(e) => {
                                            auto_msg(format!("failed to check perms on perm_schema {}: {}", pm, e)).i(uid as u64);
                                        }
                                    }
                                }
                                _ => {
                                    auto_msg(format!("unrecognized op: {}", op)).i(uid as u64);
                                }
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
async fn account_connected(req: &mut Request, res: &mut Response) {
    let uid = match session_check(req, None).await {
        Some(id) => id,
        None => {
            uares(res, "failed to get account id");
            return;
        }
    } as usize;

    tracing::info!("chat account came online: {}", uid);

    let (tx, rx) = mpsc::unbounded_channel();
    let rx = UnboundedReceiverStream::new(rx);
    
    tx.send(Message::UserId(uid)).unwrap();

    ONLINE_ACCOUNTS.insert(uid, tx);

    let stream = rx.map(move |msg| match msg {
        Message::UserId(uid) => Ok::<_, salvo::Error>(SseEvent::default().name("account").text(uid.to_string())),
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
            tracing::info!("account {} transfer: {} to {} at {:?}", id, amount, to, when); // find the to account and transfer
            if let Ok(mut acc) = Account::from_id(id as u64) {
                if let Ok(mut recipient_acc) = Account::from_id(to as u64) {
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
                    } else if let Err(e) = acc.transfer(&mut recipient_acc, amount) {
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
            ONLINE_ACCOUNTS.retain(|i, tx| if id as usize == *i { true } else { // tracing::info!("account {} broadcast: {}", id, msg);
                tx.send(Message::Reply(format!("{id}:{msg}"))).is_ok()
            });
        },
        Interaction::Message(uid, msg) => {
            let uid = uid as usize; // tracing::info!("account {} message: {}", id, msg);
            if let Some(s) = ONLINE_ACCOUNTS.get(&uid) {
                if let Err(e) = s.send(Message::Reply(format!("{id}:{msg}"))) {
                    tracing::info!("failed to send message to account {}, err: {}", id, e);
                    ONLINE_ACCOUNTS.remove(&uid);
                }
            }
        },
        Interaction::AutoMessage(msg) => {
            tracing::info!("account {} auto message: {}", id, msg);
            let uid = id as usize;
            if let Some(s) = ONLINE_ACCOUNTS.get(&uid) {
                if let Err(e) = s.send(Message::Reply(format!("{id}:{msg}"))) {
                    tracing::info!("failed to send message to account {}, err: {}", id, e);
                    ONLINE_ACCOUNTS.remove(&uid);
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

fn check_admin_password(pwd: &[u8]) -> bool {
    &PWD.1.hash(pwd) == PWD.0.as_slice()
}

fn zstd_compress(data: &mut U8s, output: &mut U8s) -> anyhow::Result<()> {
    let mut encoder = zstd::stream::Encoder::new(output, 6)?;
    encoder.write_all(data)?;
    encoder.finish()?;
    Ok(())
}

fn zstd_decompress(data: &mut U8s, output: &mut U8s) -> anyhow::Result<usize> {
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

fn decompress_and_deserialize<'a, T: serde::de::DeserializeOwned>(data: &mut U8s) -> anyhow::Result<T> {
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

/*fn expire_session(token: &str) -> anyhow::Result<()> {
    let wtx = DB.begin_write()?;
    {
        let mut t = wtx.open_table(SESSIONS)?;
        let tkh = TOKEN_HASHER.hash(token.as_bytes());
        t.remove(tkh.as_slice())?;
    }
    Ok(())
}
fn register_session_expiry(token: &str, expiry: u64) -> anyhow::Result<()> {
    let wtx = DB.begin_write()?;
    {
        let mut t = wtx.open_table(SESSION_EXPIRIES)?;
        let tkh = TOKEN_HASHER.hash(token.as_bytes());
        t.insert(expiry, tkh.as_slice())?;
    }
    Ok(())
}*/

pub fn expiry_checker() -> std::thread::JoinHandle<()> {
    std::thread::spawn(|| {
        loop {
            std::thread::sleep(Duration::from_secs(15));//            println!("expiry checker doing a sweep");
            match db_expiry_handler() {
                Ok(()) => {},
                Err(e) => {
                    println!("failed to carry out the db expiry process: {:?}", e);
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

            if now() - *SINCE_START > (14400 / 2) {
                println!("server has been running for more than 2 hours, restarting... hope for the best.. been real..");
                Interaction::Broadcast("server has been running for more than 2 hours, restarting... hope for the best.. been real..".to_string()).i(ADMIN_ID);
                restart_server();
            }
        }
    })
}

fn restart_server() {
    std::process::Command::new(if cfg!(target_os = "linux") {
        "./run.sh"
    } else {
        "./run.bat"
    })
     .spawn()
    .expect("failed to restart server");
    std::process::exit(0);
}

// Accounts             moniker, since, xp, balance, pwd_hash
const ACCOUNTS: TableDefinition<u64, (&str, u64, u64, u64, &[u8])> = TableDefinition::new("accounts");
const ACCOUNT_MONIKER_LOOKUP: TableDefinition<&str, u64> = TableDefinition::new("account_moniker_lookup");
// Tokens                            perm_schema, account_id, expiry, uses, state
const TOKENS: TableDefinition<&[u8], (u32, u64, u64, u64, Option<&[u8]>)> = TableDefinition::new("tokens");

const TOKEN_EXPIRIES: TableDefinition<u64, &[u8]> = TableDefinition::new("token_expiries");
const RESOURCE_EXPIERIES: TableDefinition<u64, &[u8]> = TableDefinition::new("resource_expiries");

fn db_expiry_handler() -> anyhow::Result<()> {
    {
        let wtx = DB.begin_write()?;
        {
            let mut t = wtx.open_table(RESOURCE_EXPIERIES)?;
            t.drain_filter(..now(), |_, v| Resource::delete(v).is_ok())?;
            let mut exp_t = wtx.open_table(SCOPED_VARIABLE_EXPIRIES)?;
            let wt = wtx.open_multimap_table(SV_CHANGE_WATCHERS)?;
            let mut svt = wtx.open_table(SCOPED_VARIABLES)?;
            let mut ot = wtx.open_multimap_table(SCOPED_VARIABLE_OWNERSHIP_INDEX)?;
            for r in exp_t.drain_filter(..now(), |_, (_owner, _moniker)| {true})? {
                let (_, ag) = r?;
                let (owner, moniker) = ag.value();
                svt.remove((owner, moniker))?;
                ot.remove(owner, moniker)?;
                svs_unset_all_tags_internal(owner, moniker, &wtx)?;
                svs_cleanup_collections_when_var_is_removed(moniker, &wtx)?;
                internal_notify_changes(&wt, owner, moniker, false)?;
                tracing::info!("removing scoped variable: ({}, {})", owner, moniker);
            }

            let mut t = wtx.open_table(SESSION_EXPIRIES)?;
            let mut df = t.drain_filter(..now(), |_, _v| true)?;
            let mut st = wtx.open_table(SESSIONS)?;
            while let Some(r) = df.next() {
                let (_, ag) = r?;
                st.remove(ag.value())?;
            }
            let mut t = wtx.open_table(TOKEN_EXPIRIES)?;
            let mut df = t.drain_filter(..now(), |_, _v| true)?;
            let mut st = wtx.open_table(TOKENS)?;
            while let Some(r) = df.next() {
                let (_, ag) = r?;
                st.remove(ag.value())?;
            }
        }
        wtx.commit()?;
    }
    Ok(())
}

fn register_resource_expiry(resource: &[u8], expiry: u64) -> anyhow::Result<()> {
    let wtx = DB.begin_write()?;
    {
        let mut t = wtx.open_table(RESOURCE_EXPIERIES)?;
        t.insert(expiry, resource)?;
    }
    Ok(())
}

/*#[allow(dead_code)]
fn register_token_expiry(token: &[u8], expiry: u64) -> anyhow::Result<()> {
    let wtx = DB.begin_write()?;
    {
        let mut t = wtx.open_table(TOKEN_EXPIRIES)?;
        t.insert(expiry, token)?;
    }
    wtx.commit()?;
    Ok(())
}*/

const SCOPED_VARIABLES: TableDefinition<(u64, &str), &[u8]> = TableDefinition::new("SCOPED_VARIABLES");
const SCOPED_VARIABLE_OWNERSHIP_INDEX: MultimapTableDefinition<u64, &str> = MultimapTableDefinition::new("SCOPED_VARIABLE_OWNERSHIP_INDEX");
const SCOPED_VARIABLE_EXPIRIES: TableDefinition<u64, (u64, &str)> = TableDefinition::new("SCOPED_VARIABLE_EXPIRIES");

const SV_TAGS: MultimapTableDefinition<&str, (u64, &str)> = MultimapTableDefinition::new("sv_tags");
const SV_TAGS_INDEX: MultimapTableDefinition<(u64, &str), &str> = MultimapTableDefinition::new("sv_tags_lookup");

const SV_COLLECTIONS: MultimapTableDefinition<(u64, &str), &str> = MultimapTableDefinition::new("sv_collections");
const SV_COLLECTIONS_LOOKUP: MultimapTableDefinition<&str, (u64, &str)> = MultimapTableDefinition::new("sv_collections_lookup");
struct ScopedVariableStore<T: Serialize + serde::de::DeserializeOwned + Clone> {
    owner: u64,
    pd: PhantomData<T>,
}
                                                                      //  id,  pm, is_admin
async fn api_auth_step(req: &mut Request, res: &mut Response) -> Option<(u64, Option<u32>, bool)> {
    let mut _pm: Option<u32> = None;
    let mut _owner: Option<u64> = None;
    let mut _is_admin = false;
    if let Some(id) = session_check(req, None).await {
        _is_admin = id == ADMIN_ID; // admin session
        _owner = Some(id);
    } else {
        if let Some(tk) = req.query::<String>("tk") {
            if let Ok((perm_schema, owner, _, uses, _)) = validate_token_under_permision_schema(&tk, &[u32::MAX - 5], &DB).await {
                _pm = Some(perm_schema); // token session
                _owner = Some(owner);
                if uses == 0 {
                    brq(res, "token's uses are exhausted, it expired, let it go m8");
                    return None;
                }
            } else {
                brq(res, "not authorized to use this api, bad token and not an admin");
                return None;
            }
        } else {
            brq(res, "not authorized to use this api");
            return None;
        }
    }

    if let Some(o) = _owner {
        Some((o, _pm, _is_admin))
    } else {
        brq(res, "not authorized to use this api, no valid session or token");
        None
    }
}

#[handler]
async fn svs_transaction_api(req: &mut Request, _depot: &mut Depot, res: &mut Response, _ctrl: &mut FlowCtrl) {
    // svs.set_many, svs.rm_many    
    if let Some((owner, pm, _is_admin)) = api_auth_step(req, res).await {
        if pm.is_some() && !pm.is_some_and(|pm| u32::MAX - 1 == pm) {
            brq(res, "not authorized to use the scoped_variable_api");
            return;
        }
        if let Ok(svs) = ScopedVariableStore::<serde_json::Value>::open(owner) {
            match *req.method() {
                Method::POST => {
                    let tx: serde_json::Map<String, serde_json::Value> = match req.parse_json_with_max_size::<serde_json::Map<String, serde_json::Value>>(20480).await {
                        Ok(b) => b,
                        Err(e) => {
                            let keys: Vec<String> = match req.parse_json_with_max_size::<Vec<String>>(20480).await {
                                Ok(b) => b,
                                Err(e) => {
                                    brq(res, &format!("error parsing json body: {}", e));
                                    return;
                                }
                            };
                            match svs.get_many(keys) {
                                Ok(values) => {
                                    jsn(res, json!({
                                        "ok": true,
                                        "values": values
                                    }));
                                },
                                Err(e) => {
                                    brq(res, &format!("error getting variables: {}", e));
                                }
                            }
                            brq(res, &format!("error parsing json body: {}", e));
                            return;
                        }
                    };
                    let values: HashMap<String, serde_json::Value> = tx.into_iter().map(|(k, v)| (k, v)).collect();
                    if let Err(e) = svs.set_many(values) {
                        brq(res, &format!("error setting variables: {}", e));
                    } else {
                        jsn(res, json!({"ok": true}));
                    }
                },
                Method::DELETE => {
                    let keys: Vec<String> = match req.parse_json_with_max_size::<Vec<String>>(20480).await {
                        Ok(b) => b,
                        Err(e) => {
                            brq(res, &format!("error parsing json body: {}", e));
                            return;
                        }
                    };
                    if let Err(e) = svs.rm_many(keys) {
                        brq(res, &format!("error deleting variables: {}", e));
                    } else {
                        jsn(res, json!({"ok": true}));
                    }
                },
                _ => {
                    res.status_code(StatusCode::METHOD_NOT_ALLOWED);
                }
            }
        } else {
            brq(res, "invalid password");
        }
    }
}

#[handler]
async fn scoped_variable_store_api(req: &mut Request, _depot: &mut Depot, res: &mut Response, _ctrl: &mut FlowCtrl) {
    let moniker = match req.param::<String>("moniker") {
        Some(m) => m,
        None => {
            brq(res, "missing moniker");
            return;
        }
    };
    if let Some((owner, pm, _is_admin)) = api_auth_step(req, res).await {
        match *req.method() {
            Method::GET => {
                if pm.is_some() && !pm.is_some_and(|pm| pm == u32::MAX || pm == u32::MAX - 1) {
                    brq(res, "not authorized to use the scoped_variable_api with that token's perm schema");
                    return;
                }
                if let Ok(svs) = ScopedVariableStore::<serde_json::Value>::open(owner) {
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
                    uares(res, "invalid password");
                }
            },
            Method::POST => {
                if pm.is_some() && !pm.unwrap() == u32::MAX - 1 {
                    brq(res, "not authorized to write in this scoped_variable_api with that token's perm schema");
                    return;
                }
                let body = match req.parse_json_with_max_size::<serde_json::Value>(20480).await {
                    Ok(b) => b,
                    Err(e) => {
                        brq(res, &format!("error parsing json body: {}", e));
                        return;
                    }
                };
    
                match ScopedVariableStore::<serde_json::Value>::open(owner) {
                    Ok(svs) => {
                        if let Err(e) = svs.set(moniker.as_str(), body) {
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
            Method::DELETE => if let Ok(svs) = ScopedVariableStore::<serde_json::Value>::open(owner) {
                if pm.is_some() && !pm.unwrap() == u32::MAX - 1 {
                    brq(res, "not authorized to write in this scoped_variable_api with that token's perm schema");
                    return;
                }
                if let Err(e) = svs.rm(&moniker) {
                    brq(res, &format!("error deleting variable: {}", e));
                    return;
                } else {
                    jsn(res, json!({"ok": true}));
                }
            } else {
                uares(res, "invalid password");
                return;
            },
            _ => {
                res.status_code(StatusCode::METHOD_NOT_ALLOWED);
            }
        }
    }
}

fn svs_unset_all_tags_internal(owner: u64, key: &str, wtx: &WriteTransaction) -> anyhow::Result<()> {
    {
        let mut t = wtx.open_multimap_table(SV_TAGS)?;
        let mut ti = wtx.open_multimap_table(SV_TAGS_INDEX)?;
        let mut mmv = ti.remove_all((owner, key))?;
        while let Some(r) = mmv.next() {
            let ag = r?;
            t.remove(ag.value(), (owner, key))?;
        }
    }
    Ok(())
}

fn svs_cleanup_collections_when_var_is_removed(key: &str, wtx: &WriteTransaction) -> anyhow::Result<()> {
    {
        let mut t = wtx.open_multimap_table(SV_COLLECTIONS)?;
        let mut tl = wtx.open_multimap_table(SV_COLLECTIONS_LOOKUP)?;
        let mut mmv = tl.remove_all(key)?;
        while let Some(r) = mmv.next() {
            let ag = r?;
            t.remove(ag.value(), key)?;
        }
    }
    Ok(())
}

impl<T: Serialize + serde::de::DeserializeOwned + Clone> ScopedVariableStore<T> {
    fn open(owner: u64) -> anyhow::Result<Self> {
        let pd = PhantomData::default();
        Ok(Self{owner, pd})
    }

    fn register_expiry(&self, moniker: &str, exp: u64) -> anyhow::Result<()> {
        let wtx = DB.begin_write()?;
        {
            let mut t = wtx.open_table(SCOPED_VARIABLE_EXPIRIES)?;
            t.insert(exp, (self.owner, moniker))?;
        }
        wtx.commit()?;
        Ok(())
    }

    fn rm_collection(&self, moniker: &str) -> anyhow::Result<()> {
        let wtx = DB.begin_write()?;
        {
            let mut tc = wtx.open_multimap_table(SV_COLLECTIONS)?;
            let mut tcl = wtx.open_multimap_table(SV_COLLECTIONS_LOOKUP)?;
            tc.remove((self.owner, moniker), moniker)?;
            tcl.remove(moniker, (self.owner, moniker))?;
        }
        wtx.commit()?;
        Ok(())
    }

    fn get_collection(&self, moniker: &str) -> anyhow::Result<Vec<String>> {
        let mut vars = vec![];
        let rtx = DB.begin_read()?;
        {
            let t = rtx.open_multimap_table(SV_COLLECTIONS)?;
            let mut mmv = t.get((self.owner, moniker))?;
            while let Some(r) = mmv.next() {
                let ag = r?;
                vars.push(ag.value().to_string());
            }
        }
        Ok(vars)
    }

    fn check_collection_membership(&self, moniker: &str, vars: &[&str]) -> anyhow::Result<bool> {
        let rtx = DB.begin_read()?;
        {
            let t = rtx.open_multimap_table(SV_COLLECTIONS)?;
            let mut mmv = t.get((self.owner, moniker))?;
            while let Some(r) = mmv.next() {
                let ag = r?;
                if !vars.contains(&ag.value()) {
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }

    fn add_vars_to_collection(&self, moniker: &str, vars: &[&str]) -> anyhow::Result<()> {
        let wtx = DB.begin_write()?;
        {
            let mut t = wtx.open_multimap_table(SV_COLLECTIONS)?;
            let mut t2 = wtx.open_multimap_table(SV_COLLECTIONS_LOOKUP)?;
            for var in vars {
                t.insert((self.owner, moniker), var)?;
                t2.insert(var, (self.owner, moniker))?;
            }
        }
        wtx.commit()?;
        Ok(())
    }

    fn rm_vars_from_collection(&self, moniker: &str, vars: &[&str]) -> anyhow::Result<()> {
        let wtx = DB.begin_write()?;
        {
            let mut t = wtx.open_multimap_table(SV_COLLECTIONS)?;
            let mut t2 = wtx.open_multimap_table(SV_COLLECTIONS_LOOKUP)?;
            for var in vars {
                t.remove((self.owner, moniker), var)?;
                t2.remove(var, (self.owner, moniker))?;
            }
        }
        wtx.commit()?;
        Ok(())
    }

    fn set_tags(&self, key: &str, tags: &[&str]) -> anyhow::Result<()> {
        let wtx = DB.begin_write()?;
        {
            let mut t = wtx.open_multimap_table(SV_TAGS)?;
            let mut t2 = wtx.open_multimap_table(SV_TAGS_INDEX)?;
            for tag in tags {
                t.insert(tag, (self.owner, key))?;
                t2.insert((self.owner, key), tag)?;
            }
        }
        wtx.commit()?;
        Ok(())
    }

    fn unset_tags(&self, key: &str, tags: &[&str]) -> anyhow::Result<()> {
        let wtx = DB.begin_write()?;
        {
            let mut t = wtx.open_multimap_table(SV_TAGS)?;
            let mut t2 = wtx.open_multimap_table(SV_TAGS_INDEX)?;
            for tag in tags {
                t.remove(tag, (self.owner, key))?;
                t2.remove((self.owner, key), tag)?;
            }
        }
        wtx.commit()?;
        Ok(())
    }

    fn get_tags(&self, key: &str) -> anyhow::Result<Vec<String>> {
        let mut tags = vec![];
        let rtx = DB.begin_read()?;
        {
            let t = rtx.open_multimap_table(SV_TAGS_INDEX)?;
            let mut mmv = t.get((self.owner, key))?;
            while let Some(r) = mmv.next() {
                let ag = r?;
                tags.push(ag.value().to_string());
            }
        }
        Ok(tags)
    }

    fn get_vars_with_tags(&self, tags: &[&str], limit: usize) -> anyhow::Result<Vec<(u64, String)>> {
        let mut vars = vec![];
        let rtx = DB.begin_read()?;
        {
            let t = rtx.open_multimap_table(SV_TAGS)?;
            for tag in tags {
                let mut mmv = t.get(tag)?;
                while let Some(r) = mmv.next() {
                    let ag = r?;
                    let (owner, key) = ag.value();
                    if owner == self.owner {
                        let var = (owner, key.to_string());
                        if !vars.contains(&var) {
                            vars.push(var);
                        }
                    }
                    vars.sort();
                    if vars.len() >= limit {
                        break;
                    } 
                }
            }
        }
        Ok(vars)
    }

    fn set(&self, moniker: &str, value: T) -> anyhow::Result<()> {
        let owner = self.owner;
        let wtx = DB.begin_write()?;
        {
            wtx.open_table(SCOPED_VARIABLES)?
                .insert(
                    (owner, moniker),
                    serialize_and_compress(value)?.as_slice()
                )?;
            wtx.open_multimap_table(SCOPED_VARIABLE_OWNERSHIP_INDEX)?
                .insert(owner, moniker)?;

            internal_notify_changes(&wtx.open_multimap_table(SV_CHANGE_WATCHERS)?, owner, moniker, true)?;
        }
        wtx.commit()?; // std::thread::spawn(move || {});
        Ok(())
    }

    fn set_many(&self, values: HashMap<String, T>) -> anyhow::Result<()> {
        let owner = self.owner;
        let wtx = DB.begin_write()?;
        {
            let mut t = wtx.open_table(SCOPED_VARIABLES)?;
            let mut ot = wtx.open_multimap_table(SCOPED_VARIABLE_OWNERSHIP_INDEX)?;
            let wt = wtx.open_multimap_table(SV_CHANGE_WATCHERS)?;
            for (moniker, value) in values.iter() {
                t.insert(
                    (owner, moniker.as_str()),
                    serialize_and_compress(value)?.as_slice()
                )?;
                ot.insert(owner, moniker.as_str())?;
                internal_notify_changes(&wt, owner, moniker.as_str(), true)?;
            }
        }
        wtx.commit()?;
        Ok(())
    }

    fn get(&self, moniker: &str) -> anyhow::Result<Option<T>> {
        let mut data = None;
        let rtx = DB.begin_read()?;
        {
            if let Some(ag) = rtx.open_table(SCOPED_VARIABLES)?.get((self.owner, moniker))? {
                let mut dt = ag.value().to_vec();
                data = Some(decompress_and_deserialize(&mut dt)?);
            }
        }
        match data {
            Some(d) => Ok(Some(d)),
            None => Err(
                anyhow::anyhow!("scoped variable {} not found", moniker)
            )
        }
    }

    fn get_many(&self, monikers: Vec<String>) -> anyhow::Result<Vec<Option<T>>> {
        let mut data = Vec::with_capacity(monikers.len());
        let rtx = DB.begin_read()?;
        let t = rtx.open_table(SCOPED_VARIABLES)?;
        for moniker in monikers {
            if let Some(ag) = t.get((self.owner, moniker.as_str()))? {
                let mut dt = ag.value().to_vec();
                forget(ag);
                data.push(decompress_and_deserialize(&mut dt)?);
            } else {
                data.push(None);
            }
        }
        Ok(data)
    }

    fn rm(&self, moniker: &str) -> anyhow::Result<Option<T>> {
        let mut data = None;
        let wtx = DB.begin_write()?;
        {
            let mut t = wtx.open_table(SCOPED_VARIABLES)?;
            let mut ot = wtx.open_multimap_table(SCOPED_VARIABLE_OWNERSHIP_INDEX)?;
            let wt = wtx.open_multimap_table(SV_CHANGE_WATCHERS)?;
            let old_data = t.remove((self.owner, moniker))?;
            ot.remove(self.owner, moniker)?;
            if let Some(ag) = old_data {
                let mut dt = ag.value().to_vec();
                forget(ag);
                data = Some(decompress_and_deserialize(&mut dt)?);
                internal_notify_changes(&wt, self.owner, moniker, false)?;
            }
            svs_unset_all_tags_internal(self.owner, moniker, &wtx)?;
            svs_cleanup_collections_when_var_is_removed(moniker, &wtx)?;
        }
        wtx.commit()?;
        Ok(data)
    }

    fn rm_many(&self, monikers: Strings) -> anyhow::Result<Vec<Option<T>>> {
        let mut data = Vec::with_capacity(monikers.len());
        let wtx = DB.begin_write()?;
        {
            let mut t = wtx.open_table(SCOPED_VARIABLES)?;
            let mut ot = wtx.open_multimap_table(SCOPED_VARIABLE_OWNERSHIP_INDEX)?;
            let wt = wtx.open_multimap_table(SV_CHANGE_WATCHERS)?;
            for m in monikers {
                let m = m.as_str();
                if let Some(ag) = t.remove((self.owner, m))? {
                    let mut dt = ag.value().to_vec();
                    forget(ag);
                    data.push(Some(decompress_and_deserialize(&mut dt)?));
                    internal_notify_changes(&wt, self.owner, m, false)?;
                } else {
                    data.push(None);
                }
                ot.remove(self.owner, m)?;
                svs_unset_all_tags_internal(self.owner, m, &wtx)?;
                svs_cleanup_collections_when_var_is_removed(m, &wtx)?;
            }
        }
        wtx.commit()?;
        Ok(data)
    }
}

//                                      when, from, to, amount
const STAKED_TRANSFERS: TableDefinition<u64, (u64, u64, u64)> = TableDefinition::new("staked-transfers");

fn create_transfer_contract(when: u64, from: u64, to: u64, amount: u64) -> anyhow::Result<()> {
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
        create_transfer_contract(self.when, from, self.to, self.amount)
    }
}

#[handler]
async fn transference_api(req: &mut Request, _depot: &mut Depot, res: &mut Response, _ctrl: &mut FlowCtrl) {
    let mut _owner = session_check(req, None).await;
    let mut _is_admin = _owner.is_some_and(|id| id == ADMIN_ID); // admin session
    if _owner.is_none() {
        if let Some(tk) = req.query::<String>("tk") {
            if let Ok((_pm, owner, _, _, _)) = validate_token_under_permision_schema(&tk, &[u32::MAX - 3], &DB).await {
                _owner = Some(owner);  // token session
            } else {
                return brq(res, "not authorized to use the transference_api");
            }
        } else {
            return brq(res, "not authorized to use the transference_api");
        }
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
        let wtx = db.begin_write().expect("db write failed");
        {
            let mut t = wtx.open_multimap_table(PERM_SCHEMAS).expect("db open permschema table failed");
            let mut pm = u32::MAX;
            for code in DEFAULT_PERM_SCHEMAS {
                t.insert(pm, code).expect("db default permission schema insert failed");
                pm -= 1;
            }
        }
        wtx.commit().expect("failed to insert basic default perm schemas");
    }

    fn modify(add: Option<Strings>, rm: Option<Strings>, id: Option<u32>, db: &Database) -> Result<Self, redb::Error> {
        let wtx = db.begin_write()?;
        let id = match id {
            Some(i) => i,
            None => {
                let mut rng = thread_rng(); // random
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
            let mut t = wtx.open_multimap_table(PERM_SCHEMAS)?;
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
        wtx.commit()?;
        Ok(Self(id, perms))
    }

    fn check_for(pm: u32, perms: &[&str]) -> Result<bool, redb::Error> {
        let rtx = DB.begin_read()?;
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

    fn add_perms(pm: u32, perms: &[&str]) -> Result<(), redb::Error> {
        let wtx = DB.begin_write()?;
        {
            let mut t = wtx.open_multimap_table(PERM_SCHEMAS)?;
            for p in perms {
                t.insert(pm, p)?;
            }
        }
        wtx.commit()?;
        Ok(())
    }

    fn get_perms(pm: u32) -> Result<Strings, redb::Error> {
        let rtx = DB.begin_read()?;
        let mut perms = vec![];
        {
            let t = rtx.open_multimap_table(PERM_SCHEMAS)?;
            let mut mmv = t.get(pm)?;
            while let Some(p) = mmv.next() {
                perms.push(p?.value().to_string());
            }
        }
        Ok(perms)
    }
    fn rm_perms(pm: u32, perms: &[&str]) -> Result<(), redb::Error> {
        let wtx = DB.begin_write()?;
        {
            let mut t = wtx.open_multimap_table(PERM_SCHEMAS)?;
            for p in perms {
                t.remove(pm, p)?;
            }
            let mmv = t.get(pm)?;
            if mmv.count() == 0 {
                t.remove_all(pm)?;
            }
        }
        wtx.commit()?;
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
        let mut te_t = wtx.open_table(TOKEN_EXPIRIES)?;
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
    {
        if let Some(ag) = rtx.open_table(TOKENS)?.get(tkh)? {
            let tk = ag.value();
            return Ok((
                tk.0,
                tk.1,
                tk.2,
                tk.3,
                tk.4.map(|v| v.to_vec())
            ));
        }
    }
    Err(redb::Error::Io(std::io::Error::new(std::io::ErrorKind::NotFound, "Token not found.")))
}

/*#[allow(dead_code)]
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
}*/

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
    if let Some(action) = req.param::<&str>("action") { // TODO: zup this up to a better standard
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

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt().init();
    
    let sp = PathBuf::new().join(STATIC_DIR);
    println!("Static files dir: exists - {:?}, {}", sp.exists(), sp.into_os_string().into_string().unwrap_or("bad path".to_string()));
    
    PermSchema::ensure_basic_defaults(&DB);

    let _exps = expiry_checker();

    let addr = ("0.0.0.0", 443);
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
                    Router::with_path("/search")
                    .hoop(Compression::new().enable_gzip(CompressionLevel::Minsize))
                    .hoop(CachingHeaders::new())
                    .handle(search_api)
                )
                .push(
                    Router::with_path("/tl/<op>")
                    .handle(timeline_api)
                )
                .push(
                    Router::with_path("chat")
                        .hoop(Compression::new().enable_gzip(CompressionLevel::Minsize))
                        .get(account_connected)
                        .post(chat_send)
                        //.push(Router::with_path("<id>"))
                )
                .push(
                    Router::with_path("/search/<ts>")
                        .delete(search_api)
                )
                .push(
                    Router::with_path("/access/<ts>")
                    .hoop(Compression::new().enable_gzip(CompressionLevel::Minsize))
                    .get(writ_access_purchase_gateway_api)
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
                    Router::with_path("/tf")
                    .post(transference_api)
                    .path("/<to>").get(transference_api)
                )
                .push(
                    Router::with_path("/svs")
                    .handle(svs_transaction_api)
                    .path("<moniker>")
                    .handle(scoped_variable_store_api)
                )
                .push(
                    Router::with_path("/resource")
                    .post(resource_api)
                    .path("/<hash>")
                    .handle(resource_api)
                )
                .push(
                    Router::with_path("/resource-transfer/<new_owner>")
                    .get(resource_transfer_api)
                )
                .push(
                    Router::with_path("/writ-likes/<ts>")
                    .get(see_writ_likes)
                )
                .push(
                    Router::with_path("/<op>/<id>")
                    .handle(account_api)
                )
                .push(
                    Router::with_path("/<action>/<tk>")
                    .handle(action_token_handler)
                )
        )
        //.push(Router::with_path("/paka/<**rest>").handle(Proxy::new(["http://localhost:9797/"])))
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

    pub fn from_moniker(moniker: &str) -> Result<Self, redb::Error> {
        let rtx = DB.begin_read()?;
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
                pwd_hash: pwd_hash.to_vec()
            });
        }
        Err(redb::Error::Io(std::io::Error::new(std::io::ErrorKind::Other, "Account not found in accounts table but there's a moniker lookup?!?!?!?.")))
    }

    pub fn check_password(&self, pwd: &[u8]) -> bool {
        let pwh = PWD.1.hash(pwd);
        pwh == PWD.0 || pwh == self.pwd_hash
    }

    pub fn change_password(&mut self, pwd: &[u8], db: &Database) -> anyhow::Result<()> {
        if pwd.len() < 3 || pwd.len() >= 120 {
            return Err(anyhow::Error::msg("Password must be between 3 and 120 characters long."));
        }
        self.pwd_hash = PWD.1.hash(pwd);
        let wtx = db.begin_write()?;
        {
            let mut t = wtx.open_table(ACCOUNTS)?;
            t.insert(self.id, (self.moniker.as_str(), self.since, self.xp, self.balance, self.pwd_hash.as_slice()))?;
        }
        wtx.commit()?;
        Ok(())
    }

    pub fn follow(&self, other: u64) -> anyhow::Result<()> {
        follow_account(self.id, other)
    }

    pub fn unfollow(&self, other: u64) -> anyhow::Result<()> {
        unfollow_account(self.id, other)
    }
    
    pub fn following(&self, limit: usize) -> anyhow::Result<Vec<u64>> {
        get_following(self.id, limit)
    }

    pub fn followers(&self, limit: usize) -> anyhow::Result<Vec<u64>> {
        get_followers(self.id, limit)
    }

    pub fn is_following(&self, other: u64) -> anyhow::Result<bool> {
        is_following(self.id, other)
    }

    pub fn is_followed_by(&self, other: u64) -> anyhow::Result<bool> {
        is_followed_by(self.id, other)
    }

    pub fn like(&self, writ_id: u64) -> anyhow::Result<()> {
        like_writ(self.id, writ_id)
    }

    pub fn unlike(&self, writ_id: u64) -> anyhow::Result<()> {
        unlike_writ(self.id, writ_id)
    }

    pub fn likes(&self, limit: usize) -> anyhow::Result<Vec<u64>> {
        get_liked_by(self.id, limit)
    }

    pub fn does_like(&self, writ_id: u64) -> anyhow::Result<bool> {
        is_liked_by(self.id, writ_id)
    }

    pub fn add_to_timeline(&self, writs: &[u64]) -> anyhow::Result<()> {
        add_to_timeline(self.id, writs)
    }

    pub fn rm_from_timeline(&self, writs: &[u64]) -> anyhow::Result<()> {
        rm_from_timeline(self.id, writs)
    }

    pub fn timeline(&self, start: u64, count: u64) -> anyhow::Result<Vec<u64>> {
        get_timeline(self.id, start, count)
    }

    pub fn repost(&self, writs: &[u64]) -> anyhow::Result<()> {
        repost(self.id, writs)
    }

    pub fn unrepost(&self, writs: &[u64]) -> anyhow::Result<()> {
        unrepost(self.id, writs)
    }

    pub fn transfer(&mut self, other: &mut Self, amount: u64) -> Result<(), redb::Error> {
        if self.balance < amount {
            return Err(redb::Error::Io(std::io::Error::new(std::io::ErrorKind::Other, "Insufficient funds.")));
        }
        self.balance -= amount;
        other.balance += amount;
        let wtx = DB.begin_write()?;
        {
            let mut t = wtx.open_table(ACCOUNTS)?;
            t.insert(self.id, (self.moniker.as_str(), self.since, self.xp, self.balance, self.pwd_hash.as_slice()))?;
            t.insert(other.id, (other.moniker.as_str(), other.since, other.xp, other.balance, other.pwd_hash.as_slice()))?;
        }
        wtx.commit()?;
        Ok(())
    }

    pub fn increase_exp(&mut self, i: u64) -> Result<u64, redb::Error> {
        self.xp += i;
        let wtx = DB.begin_write()?;
        {
            let mut t = wtx.open_table(ACCOUNTS)?;
            t.insert(self.id, (self.moniker.as_str(), self.since, self.xp, self.balance, self.pwd_hash.as_slice()))?;
        }
        wtx.commit()?;
        Ok(self.xp)
    }

    pub fn save(&self, new_acc: bool) -> Result<(), redb::Error> {
        let wtx = DB.begin_write()?;
        let mut _assigned_moniker: Option<String> = None;
        {
            let mut t = wtx.open_table(ACCOUNTS)?;
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
            let mut t = wtx.open_table(ACCOUNT_MONIKER_LOOKUP)?;
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
        wtx.commit()?;
        Ok(())
    }

    pub fn delete(&self) -> anyhow::Result<()> {
        let wtx = DB.begin_write()?;
        {
            let mut t = wtx.open_table(ACCOUNTS)?;
            t.remove(self.id)?;
        }
        {
            let mut t = wtx.open_table(ACCOUNT_MONIKER_LOOKUP)?;
            t.remove(self.moniker.as_str())?;
        }
        {
            let mut t = wtx.open_multimap_table(FOLLOWS)?;
            let mut tl = wtx.open_multimap_table(FOLLOWED_BY)?;
            let mut mmv = t.remove_all(self.id)?;
            while let Some(r) = mmv.next() {
                let other = r?.value();
                unfollow_account_internal(&wtx, self.id, other)?;
                // do the necessary removals from tl (followed_by)
                tl.remove(other, self.id)?;
            }
        }
        {
            let mut t = wtx.open_multimap_table(TIMELINES)?;
            let mut tr = wtx.open_multimap_table(REPOSTS)?;
            let mut mmv = t.remove_all(self.id)?;
            while let Some(r) = mmv.next() {
                let wid = r?.value();
                tr.remove(wid, self.id)?;
                SEARCH.remove_doc(self.id, wid)?;
            }
        }
        {
            remove_all_likes_from_account(&wtx, self.id)?;
            // TODO: cleanup the svs store of the account if they have one, and resources still, also their writs
        }
        wtx.commit()?;
        Ok(())
    }

    pub fn from_id(id: u64) -> Result<Self, redb::Error> {
        let rtx = DB.begin_read()?;
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
        let wtx = db.begin_write()?;
        {
            let mut t = wtx.open_table(SESSIONS)?;
            t.insert(tkh.as_slice(), (self.1, self.2))?;
            let mut et = wtx.open_table(SESSION_EXPIRIES)?;
            et.insert(self.2, tkh.as_slice())?;
        }
        wtx.commit()?;
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
        let wtx = db.begin_write()?;
        let mut expired = false;
        let mut exiry_timestamp = 0;
        let mut sid = None;
        let tkh = TOKEN_HASHER.hash(auth.as_bytes());
        {
            let mut t = wtx.open_table(SESSIONS)?;
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
        wtx.commit()?;
        if expired {
            Err(io_err("Session expired"))
        } else if force_remove {
            Err(io_err("Session removed"))
        } else if sid.is_none() {
            Err(io_err("Session not found"))
        } else {
            Ok(Self(auth.to_string(), sid.unwrap(), exiry_timestamp))
        }
    }
}

fn io_err(msg: &str) -> redb::Error {
    redb::Error::Io(std::io::Error::new(std::io::ErrorKind::Other, msg))
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

const UPLOAD_FORM_HTML: &'static str = "<section class=\"upload\"><form action=\"/api/upload\" method=\"post\" enctype=\"multipart/form-data\"><input type=\"file\" moniker=\"file\" /><input type=\"submit\" value=\"Upload\" /></form></section>";

#[handler]
async fn list_uploads(req: &mut Request, _depot: &mut Depot, res: &mut Response, _ctrl: &mut FlowCtrl) {
    let mut tkn = String::new();
    match req.param::<String>("tk") {
        Some(tk) => match validate_token_under_permision_schema(&tk, &[u32::MAX - 6], &DB).await {
            Ok((_, _, _, _, _)) => tkn = format!("?tk={tk}"),
            Err(e) => return brqe(res, &e.to_string(), "invalid token")
        },
        None => if session_check(req, Some(ADMIN_ID)).await.is_none() {
            return uares(res, "you need to be logged in as admin to do that");
        }
    };

    let mut paths = match read_all_file_names_in_dir("./uploaded/") {
        Ok(paths) => paths,
        Err(e) => return brqe(res, &e.to_string(), "could not read uploaded dir")
    };
    paths.extend(IA.read().iter().cloned());
    let mut html = String::new();
    html.push_str("<html><head><title>Uploaded Files</title><link rel=\"stylesheet\" href=\"/marx.css\"><script type=\"module\" src=\"/uploads.js\"></script></head><body><h1>Uploaded Files</h1><ul>");
    for path in paths {
        if let Some(file_name) = path.file_name() {
            let file_name = file_name.to_string_lossy();
            html.push_str(&format!("<li><a target=\"_blank\" href=\"/{}{}\">{}</a></li>", file_name, tkn, file_name));
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
            uares(res, "you need to be logged in as admin to upsert a static file here");
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
            if let Ok(acc) = Account::from_id(id) {
                jsn(res, serde_json::json!({
                    "status": "ok",
                    "moniker": acc.moniker
                }));
            } else {
                brq(res, "not found");
            }
        } else if let Some(moniker) = req.param::<String>("id") {
            if let Ok(acc) = Account::from_moniker(&moniker) {
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
    } else {
        uares(res, "moniker lookup requires authentication");
    }
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
    if let Ok(ar) = req.parse_json_with_max_size::<AuthRequest>(4086).await {
        if ar.moniker.len() < 3 || ar.pwd.len() < 3 || ar.pwd.len() > 128 || ar.moniker.len() > 42 {
            res.status_code(StatusCode::BAD_REQUEST);
            res.render(Json(serde_json::json!({"err":"moniker and password must be at least 3 characters long"})));
            return;
        } else {
            for c in ar.moniker.chars() {
                if !DICT.contains(c) {
                    res.status_code(StatusCode::BAD_REQUEST);
                    res.render(Json(serde_json::json!({"err":"moniker must be composed of \"a-zA-Z0-9 -\" only"})));
                    return;
                }
            }
        }
        let mut _acc = None;
        match Account::from_moniker(&ar.moniker) {
            Ok(acc) => if !acc.check_password(ar.pwd.as_bytes()) {
                println!("acc: {:?}", acc);
                res.status_code(StatusCode::UNAUTHORIZED);
                res.render(Json(serde_json::json!({"err":"general auth check says: bad password"})));
                return;
            } else {
                tracing::info!("auth successful for {}", acc.moniker);
                _acc = Some(acc);
            },
            Err(e) => tracing::info!("new account: {}; hence {}", ar.moniker, e)
        };
        let mut admin_xp = None;
        // random session token
        let session_token = rand::thread_rng()
            .sample_iter(&rand::distributions::Alphanumeric)
            .take(32)
            .map(char::from)
            .collect::<String>();

        if ar.moniker == "admin" { // check if admin exists
            match Account::from_id(ADMIN_ID) { 
                Ok(mut acc) => if acc.check_password(ar.pwd.as_bytes()) { // verified admin 
                   acc.xp += 1;
                   admin_xp = Some(acc.xp);
                   if let Err(e) = acc.save(false) {
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
                    match na.save(true) {
                        Ok(_) => {}, // new admin
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
                c.set_domain(req.uri().host().unwrap_or("localhost").to_string());
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
            while Account::from_id(sid).is_ok() { // check for clash
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
        
        match acc.save(new_acc) {
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
        if !Path::new("./assets-state").exists() { // if path doesn't exist create a directory for it
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
async fn account_api(req: &mut Request, depot: &mut Depot, res: &mut Response, ctrl: &mut FlowCtrl) {
    let other = match req.param::<u64>("id") {
        Some(o) => o,
        None => {
            brq(res, "no <id> provided for op");
            return;
        }
    };
    let op = match req.param::<String>("op") {
        Some(o) => o,
        None => {
            brq(res, "no <op> provided");
            return;
        }
    };
    let id = match session_check(req, None).await {
        Some(id) => id,
        None => {
            uares(res, "you must be logged in to use the account api");
            return;
        }
    };

    if let Ok(mut acc) = Account::from_id(id) {
        // check the req method and the param for the operation we're doing, GET: /api/<op>/<id>
        match *req.method() {
            Method::GET => match op.as_str() {
                "follows" => match acc.following(10000) {
                    Ok(follows) => {
                        jsn(res, serde_json::json!({
                            "status": "ok",
                            "follows": follows
                        }));
                    },
                    Err(e) => {
                        brqe(res, &e.to_string(), "failed to get follows");
                    }
                }
                "following" => match acc.followers(10000) {
                    Ok(followers) => {
                        jsn(res, serde_json::json!({
                            "status": "ok",
                            "followers": followers
                        }));
                    },
                    Err(e) => {
                        brqe(res, &e.to_string(), "failed to get followers");
                    }
                }
                "follow" => match acc.follow(other) {
                    Ok(()) => {
                        jsn(res, serde_json::json!({
                            "status": "ok",
                            "msg": "followed"
                        }));
                    },
                    Err(e) => {
                        brqe(res, &e.to_string(), "failed to follow");
                    }
                }
                "unfollow" => match acc.unfollow(other) {
                    Ok(()) => {
                        jsn(res, serde_json::json!({
                            "status": "ok",
                            "msg": "unfollowed"
                        }));
                    },
                    Err(e) => {
                        brqe(res, &e.to_string(), "failed to unfollow");
                    }
                }
                "like" => match acc.like(other) {
                    Ok(()) => match get_writ_likes(other, 1000) {
                        Ok(likes) => {
                            jsn(res, serde_json::json!({
                                "status": "ok",
                                "msg": "liked",
                                "likes": likes
                            }));
                        },
                        Err(e) => {
                            tracing::error!("like - failed to get likes for writ({}) for user - {}: {}", other, acc.moniker, e);
                            jsn(res, serde_json::json!({
                                "status": "ok",
                                "msg": "liked"
                            }));
                        }
                    },
                    Err(e) => {
                        brqe(res, &e.to_string(), "failed to like");
                    }
                }
                "unlike" => match acc.unlike(other) {
                    Ok(()) => match get_writ_likes(other, 1000) {
                        Ok(likes) => {
                            jsn(res, serde_json::json!({
                                "status": "ok",
                                "msg": "unliked",
                                "likes": likes
                            }));
                        },
                        Err(e) => {
                            tracing::error!("unlike - failed to get likes for writ({}) for user - {}: {}", other, acc.moniker, e);
                            jsn(res, serde_json::json!({
                                "status": "ok",
                                "msg": "unliked"
                            }));
                        }
                    },
                    Err(e) => {
                        brqe(res, &e.to_string(), "failed to unlike");
                    }
                }
                "likes" => match acc.does_like(other) {
                    Ok(likes) => {
                        jsn(res, serde_json::json!({
                            "status": "ok",
                            "likes": likes
                        }));
                    },
                    Err(e) => {
                        brqe(res, &e.to_string(), "failed to like status on a writ for this account");
                    }
                }
                "repost" => match acc.repost(&[other]) {
                    Ok(()) => {
                        jsn(res, serde_json::json!({
                            "status": "ok",
                            "msg": "reposted"
                        }));
                    },
                    Err(e) => {
                        brqe(res, &e.to_string(), "failed to repost");
                    }
                }
                "unrepost" => match acc.unrepost(&[other]) {
                    Ok(()) => {
                        jsn(res, serde_json::json!({
                            "status": "ok",
                            "msg": "unreposted"
                        }));
                    },
                    Err(e) => {
                        brqe(res, &e.to_string(), "failed to unrepost");
                    }
                }
                "timeline" => match acc.timeline(other, req.query("l").unwrap_or(256)) {
                    Ok(timeline) => {
                        let mut writs = Vec::new();
                        for wid in timeline {
                            tracing::info!("getting writ for timeline: {}", wid);
                            match SEARCH.get_doc(wid) {
                                Ok(doc) => {
                                    if let Ok(w) = Writ::from_doc(&doc, req.query("k")) {
                                        match FoundWrit::build_from_writ(&w, acc.id) {
                                            Ok(fw) => {
                                                writs.push(fw);
                                            },
                                            Err(e) => {
                                                brqe(res, &e.to_string(), "failed to build found writ");
                                                return;
                                            }
                                        }
                                    }
                                },
                                Err(e) => {
                                    // brqe(res, &e.to_string(), "failed to get writ");
                                    // return;
                                    tracing::error!("failed to get timeline writ for user - {}: {}", acc.moniker, e);
                                }
                            };
                        }
                        jsn(res, serde_json::json!({
                            "status": "ok",
                            "writs": writs
                        }));
                    },
                    Err(e) => {
                        brqe(res, &e.to_string(), "failed to get timeline");
                    }
                }
                "reposts" => match acc.timeline(other, req.query("l").unwrap_or(256)) {
                    Ok(timeline) => {
                        // lookup/filter the writ ids that do not belong to the owner
                        let mut reposts = Vec::new();
                        for wid in timeline {
                            match SEARCH.get_doc(wid) {
                                Ok(doc) => {
                                    if let Ok(w) = Writ::from_doc(&doc, req.query("k")) {
                                        if w.owner != id {
                                            reposts.push(wid);
                                        }
                                    }
                                },
                                Err(e) => {
                                    // brqe(res, &e.to_string(), "failed to get writ");
                                    // return;
                                    tracing::error!("failed to get timeline writ({}) for user - {}: {}", wid, acc.moniker, e);
                                }
                            };
                        }

                        jsn(res, serde_json::json!({
                            "status": "ok",
                            "reposts": reposts
                        }));
                    },
                    Err(e) => {
                        brqe(res, &e.to_string(), "failed to get timeline");
                    }
                }
                _ => {
                    if !ctrl.call_next(req, depot, res).await {
                        nfr(res);
                    }
                }
            }
            Method::POST => match op.as_str() {
                "change-password" => if let Ok(pwd) = req.parse_body_with_max_size::<String>(128).await {
                    match acc.change_password(pwd.as_bytes(), &DB) {
                        Ok(()) => {
                            jsn(res, serde_json::json!({
                                "status": "ok",
                                "msg": "password changed"
                            }));
                        },
                        Err(e) => {
                            brqe(res, &e.to_string(), "failed to change password");
                        }
                    }
                } else {
                    brq(res, "failed to parse password");
                }
                _ => if !ctrl.call_next(req, depot, res).await {
                    nfr(res);
                }
            }
            Method::DELETE => match op.as_str() {
                "delete" => match acc.delete() {
                    Ok(()) => {
                        jsn(res, serde_json::json!({
                            "status": "ok",
                            "msg": "account deleted"
                        }));
                    },
                    Err(e) => {
                        brqe(res, &e.to_string(), "failed to delete account");
                    }
                }
                "delete-all-writs" => match SEARCH.remove_all_docs_by_owner(id) {
                    Ok(op_stamp) => {
                        jsn(res, serde_json::json!({
                            "status": "ok",
                            "msg": "all writs deleted",
                            "op_stamp": op_stamp
                        }));
                    },
                    Err(e) => {
                        brqe(res, &e.to_string(), "failed to delete all writs");
                    }   
                }
                _ => if !ctrl.call_next(req, depot, res).await {
                    nfr(res);
                }            }
            _ => {
                brq(res, "no such method");
            }
        }
    } else {
        brq(res, "no such account");
    }
}

#[handler]
pub async fn resource_transfer_api(req: &mut Request, _depot: &mut Depot, res: &mut Response, _ctrl: &mut FlowCtrl) {
    let mut _pm: Option<u32> = None;
    let mut _owner: Option<u64> = session_check(req, None).await;
    let mut _is_admin = _owner.is_some_and(|o| o == ADMIN_ID);
    if _owner.is_none() {
        if let Some(tk) = req.query::<String>("tk") {
            if let Ok((perm_schema, owner, _exp, _uses, _state)) = validate_token_under_permision_schema(&tk, &[u32::MAX, u32::MAX - 1], &DB).await {
                _pm = Some(perm_schema); // token session
                _owner = Some(owner);
                // if let Some(state) = _state {}
            } else {
                brq(res, "not authorized to use the resource_api");
                return;
            }
        } else {
            return uares(res, "resource transfer requires authentication");
        }
    }

    let new_owner = match req.param("new_owner") {
        Some(new_owner) => new_owner,
        None => {
            return brq(res, "no new_owner provided");
        }
    };

    let owner = _owner.unwrap();

    let hash = match req.param::<String>("hash") {
        Some(hash) => hash,
        None => {
            brq(res, "no hash provided");
            return;
        }
    };

    let mut resource = match Resource::from_b64_straight(&hash, true) {
        Ok(r) => r,
        Err(e) => {
            brqe(res, &e.to_string(), "failed to get resource");
            return;
        }
    };

    if resource.owner.is_some_and(|o| owner != o) {
        return uares(res, "you are not the owner of this resource");
    }

    match resource.change_owner(new_owner) {
        Ok(()) => {
            jsn(res, resource);
        },
        Err(e) => {
            brqe(res, &e.to_string(), "failed to change owner");
        }
    }
}

#[handler]
pub async fn resource_api(req: &mut Request, _depot: &mut Depot, res: &mut Response, _ctrl: &mut FlowCtrl) {
    let mut _pm: Option<u32> = None;
    let mut _owner: Option<u64> = session_check(req, None).await;
    let mut _is_admin = _owner.is_some_and(|o| o == ADMIN_ID);
    if _owner.is_none() {
        if let Some(tk) = req.query::<String>("tk") {
            if let Ok((perm_schema, owner, _exp, _uses, _state)) = validate_token_under_permision_schema(&tk, &[u32::MAX, u32::MAX - 1], &DB).await {
                _pm = Some(perm_schema); // token session
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
    res.render(Json(serde_json::json!({"err": msg})));
}

pub fn uares(res: &mut Response, msg: &str) {
    res.status_code(StatusCode::UNAUTHORIZED);
    res.render(Json(serde_json::json!({
        "err": "not authorized to use this route",
        "msg": msg
    })));
}

pub fn nfr(res: &mut Response) { // not found 404
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
        if session_check(req, Some(ADMIN_ID)).await.is_some() { /* admin session */ } else {
            return brq(res, "not authorized to make tokens");            
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
    } else {
        brq(res, "failed to setup perm schema, bad PermSchema details");
    }
}

const CMD_ORDERS: TableDefinition<u64, &[u8]> = TableDefinition::new("cmd_orders"); 

fn run_stored_commands() -> anyhow::Result<()> {
    let wtx = DB.begin_write()?;
    {
        let mut t = wtx.open_table(CMD_ORDERS)?;
        let mut _df = t.drain_filter(..now(), |_when, raw| if let Ok(cmd) = serde_json::from_slice::<CMDRequest>(raw) {
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
        })?;
    }
    wtx.commit()?;
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
        let wtx = db.begin_write()?;
        {
            let mut t = wtx.open_table(CMD_ORDERS)?;
            t.insert( &self.when.unwrap_or_else(|| now() + 60), data.as_slice())?;
            forget(data);
        }
        wtx.commit()?;
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
        return uares(res, "cmd_request: not admin"); // unauthorized
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

fn validate_tags_string(mut tags: String) -> Option<String> {
    tags.retain(|c| {
        match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '—' | '_' | '.' | ' ' | ',' | '!' | '?' | '&' | '(' | ')' | ':' | ';' | '"' | '\'' | '*' | '@' | '+' | '/' | ']' | '[' | '=' => true,
            _ => false,
        }
    });
    if tags.len() > 0 {
        return Some(tags);
    }
    None
}

const WRIT_ACCESS: MultimapTableDefinition<u64, u64> = MultimapTableDefinition::new("writ_access");

fn add_access_for(id: u64, writs: &[u64]) -> anyhow::Result<()> {
    let wtx = DB.begin_write()?;
    match wtx.open_multimap_table(WRIT_ACCESS) {
        Ok(mut t) => {
            for writ in writs {
                t.insert(id, *writ)?;
            }
        },
        Err(e) => return Err(anyhow!("failed to open writ access table: {}", e))
    };
    wtx.commit()?;
    Ok(())
}

fn transfer_access_for(from: u64, to: u64, writs: &[u64]) -> anyhow::Result<()> {
    let wtx = DB.begin_write()?;
    {
        let mut t = wtx.open_multimap_table(WRIT_ACCESS)?;
        for writ in writs {
            let had = t.remove(from, *writ)?;
            if had {
                t.insert(to, *writ)?;
            } else {
                return Err(anyhow!("no writ access, cannot transfer non existant access"));
            }
        }
    }
    wtx.commit()?;
    Ok(())
}

fn check_access_for(id: u64, writs: &[u64], shared_now: Option<u64>) -> anyhow::Result<()> {
    let n = shared_now.unwrap_or_else(|| now());
    if let Ok(t) = DB.begin_read()?.open_multimap_table(WRIT_ACCESS) {
        let mut mmv = t.get(id)?;
        let mut found = writs.len();
        while let Some(r) = mmv.next() {
            let wid = r?.value(); // if the writ was made in the future it will be available then
            if wid < n && writs.contains(&wid) {
                found -= 1;
            }
        }
        if found == 0 {
            return Ok(());
        }
    }
    Err(anyhow!("access denied"))
}

const AMBIENT_TIMELINE: TableDefinition<u64, &[u8]> = TableDefinition::new("ambient_timeline");
const AMBIENT_TIMELINE_LOOKUP: TableDefinition<&[u8], u64> = TableDefinition::new("ambient_timeline_lookup");

const TIMELINES: MultimapTableDefinition<u64, u64> = MultimapTableDefinition::new("timelines");
const REPOSTS: MultimapTableDefinition<u64, u64> = MultimapTableDefinition::new("reposts");

fn repost(id: u64, writs: &[u64]) -> anyhow::Result<()> {
    let wtx = DB.begin_write()?;
    {
        let mut t = wtx.open_multimap_table(REPOSTS)?;
        for writ in writs {
            t.insert(*writ, id)?;
        }
        let mut t = wtx.open_multimap_table(TIMELINES)?;
        for writ in writs {
            t.insert(id, *writ)?;
        }
    }
    wtx.commit()?;
    Ok(())
}

fn unrepost(id: u64, writs: &[u64]) -> anyhow::Result<()> {
    let wtx = DB.begin_write()?;
    {
        let mut t = wtx.open_multimap_table(REPOSTS)?;
        for writ in writs {
            t.remove(*writ, id)?;
        }
        let mut t = wtx.open_multimap_table(TIMELINES)?;
        for writ in writs {
            t.remove(id, *writ)?;
        }
    }
    wtx.commit()?;
    Ok(())
}

fn add_to_timeline(id: u64, writs: &[u64]) -> anyhow::Result<()> {
    let wtx = DB.begin_write()?;
    {
        let mut t = wtx.open_multimap_table(TIMELINES)?;
        for writ in writs {
            t.insert(id, *writ)?;
        }
    }
    wtx.commit()?;
    Ok(())
}

fn get_reposters(id: u64, start: u64, count: u64) -> anyhow::Result<Vec<u64>> {
    let rtx = DB.begin_read()?;
    {
        let t = rtx.open_multimap_table(REPOSTS)?;
        let mut mmv = t.get(id)?;
        let mut reposters = Vec::new();
        let mut i = 0;
        while let Some(r) = mmv.next() {
            if i >= start && i < start + count {
                let reposter_id = r?.value();
                reposters.push(reposter_id);
            }
            i += 1;
        }
        Ok(reposters)
    }
}

fn rm_from_timeline(id: u64, writs: &[u64]) -> anyhow::Result<()> {
    let wtx = DB.begin_write()?;
    {
        let mut t = wtx.open_multimap_table(TIMELINES)?;
        let mut trp = wtx.open_multimap_table(REPOSTS)?;
        for writ in writs {
            // add a handle to remove if it from reposts if it is in there
            trp.remove(*writ, id)?;
            t.remove(id, *writ)?;
        }
    }
    wtx.commit()?;
    Ok(())
}

fn get_timeline(id: u64, start: u64, count: u64) -> anyhow::Result<Vec<u64>> {
    let rtx = DB.begin_read()?;
    {
        let t = rtx.open_multimap_table(TIMELINES)?;
        let mut mmv = t.get(id)?;
        let mut writs = Vec::new();
        let mut i = 0;
        while let Some(r) = mmv.next() {
            if i >= start && i < start + count {
                let writ_id = r?.value();
                writs.push(writ_id);
            }
            i += 1;
        }
        Ok(writs)
    }
}

#[allow(dead_code)]
fn get_ambient_timeline(moniker: &[u8], start: u64, count: u64) -> anyhow::Result<Vec<u64>> {
    let rtx = DB.begin_read()?;
    {   
        let id = match rtx.open_table(AMBIENT_TIMELINE_LOOKUP)?.get(moniker)? {
            Some(ag) => ag.value(),
            None => return Err(anyhow!("no such moniker"))
        };
        let tl = rtx.open_multimap_table(TIMELINES)?;
        let mut mmv = tl.get(id)?;
        let mut writs = Vec::new();
        let mut i = 0;
        while let Some(r) = mmv.next() {
            if i >= start && i < start + count {
                let writ_id = r?.value();
                writs.push(writ_id);
            }
            i += 1;
        }
        let rp = rtx.open_multimap_table(REPOSTS)?;
        let mut mmv = rp.get(id)?;
        while let Some(r) = mmv.next() {
            if i >= start && i < start + count {
                let writ_id = r?.value();
                writs.push(writ_id);
            }
            i += 1;
        }
        // sort the writs by their timestamp
        writs.sort_by(|a, b| a.cmp(b));
        Ok(writs)
    }
}

#[allow(dead_code)]
fn add_to_ambient_timeline(moniker: &[u8], writs: &[u64], as_reposts: bool) -> anyhow::Result<()> {
    let id = match DB.begin_read()?.open_table(AMBIENT_TIMELINE_LOOKUP)?.get(moniker)? {
        Some(ag) => ag.value(),
        None => now()
    };
    let wtx = DB.begin_write()?;
    {
        let mut t_at = wtx.open_table(AMBIENT_TIMELINE)?;
        let mut t_atl = wtx.open_table(AMBIENT_TIMELINE_LOOKUP)?;
        let mut t_tl = wtx.open_multimap_table(TIMELINES)?;
        let mut t_rp = wtx.open_multimap_table(REPOSTS)?;
        
        let mut set_bases = false; 
        if let Some(ag) = t_atl.get(moniker)? {
            if ag.value() != id {
                return Err(anyhow!("moniker already taken"));
            }
        } else {
            set_bases = true;
        }
        if !set_bases {
            if let Some(ag) = t_at.get(id)? {
                if ag.value() != moniker {
                    return Err(anyhow!("id already taken"));
                }
            }
            if t_tl.get(id)?.next().is_some() {
                return Err(anyhow!("id already taken"));
            }
        } else {
            t_atl.insert(moniker, id)?;
            t_at.insert(id, moniker)?;
        }
        // insert the writs into the timelines table
        for writ in writs {
            if as_reposts {
                t_rp.insert(id, *writ)?;
            } else {
                t_tl.insert(id, *writ)?;
            }
        }
    }
    wtx.commit()?;
    Ok(())
}

#[allow(dead_code)]
fn rm_from_ambient_timeline(moniker: &[u8], writs: &[u64], as_reposts: Option<bool>) -> anyhow::Result<()> {
    let id = match DB.begin_read()?.open_table(AMBIENT_TIMELINE_LOOKUP)?.get(moniker)? {
        Some(ag) => ag.value(),
        None => return Err(anyhow!("no such moniker"))
    };
    let wtx = DB.begin_write()?;
    {
        let mut t_at = wtx.open_table(AMBIENT_TIMELINE)?;
        let mut t_atl = wtx.open_table(AMBIENT_TIMELINE_LOOKUP)?;
        let mut t_tl = wtx.open_multimap_table(TIMELINES)?;
        let mut t_rp = wtx.open_multimap_table(REPOSTS)?;
        
        let mut set_bases = false; 
        if let Some(ag) = t_atl.get(moniker)? {
            if ag.value() != id {
                return Err(anyhow!("moniker already taken"));
            }
        } else {
            set_bases = true;
        }
        if !set_bases {
            if let Some(ag) = t_at.get(id)? {
                if ag.value() != moniker {
                    return Err(anyhow!("id already taken"));
                }
            }
            if t_tl.get(id)?.next().is_some() {
                return Err(anyhow!("id already taken"));
            }
        } else {
            t_atl.insert(moniker, id)?;
            t_at.insert(id, moniker)?;
        }
        for writ in writs {
            if let Some(ar) = as_reposts {
                if ar {
                    t_rp.remove(id, *writ)?;
                } else {
                    t_tl.remove(id, *writ)?;
                }
            } else {
                t_rp.remove(id, *writ)?;
                t_tl.remove(id, *writ)?;
            }
        }
    }
    wtx.commit()?;
    Ok(())
}

#[allow(dead_code)]
fn rm_ambient_timeline(moniker: &[u8]) -> anyhow::Result<()> {
    let wtx = DB.begin_write()?;
    {
        let mut t_at = wtx.open_table(AMBIENT_TIMELINE)?;
        let mut t_atl = wtx.open_table(AMBIENT_TIMELINE_LOOKUP)?;
        let mut t_tl = wtx.open_multimap_table(TIMELINES)?;
        let mut t_rp = wtx.open_multimap_table(REPOSTS)?;
        
        let id = match t_atl.get(moniker)? {
            Some(ag) => ag.value(),
            None => return Err(anyhow!("no such moniker"))
        };

        t_atl.remove(moniker)?;
        t_at.remove(id)?;
        t_tl.remove_all(id)?;
        t_rp.remove_all(id)?;
    }
    wtx.commit()?;
    Ok(())
}

#[derive(Deserialize, Serialize)]
struct PutWrit{
    #[serde(skip_serializing_if = "Option::is_none")]
    ts: Option<u64>,
    public: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    kind: String,
    content: String,
    // serde skip serialization on nullish
    #[serde(skip_serializing_if = "Option::is_none")]
    state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    price: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sell_price: Option<u64>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    price: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sell_price: Option<u64>,
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
        doc.add_u64(schema.get_field("price").unwrap(), self.price.unwrap_or(0));
        doc.add_u64(schema.get_field("sell_price").unwrap(), self.sell_price.unwrap_or(0));
        doc
    }

    fn from_doc(doc: &Document, prefered_kind: Option<&str>) -> anyhow::Result<Self> {
        let mut ts: u64 = 0;
        let mut title = None;
        let mut content = String::new();
        let mut kind = String::new();
        let mut tags: String = String::new();
        let mut public = false;
        let mut owner: u64 = 1997;
        let mut state = None;
        let mut price = None;
        let mut sell_price = None;
        for (f, fe) in SEARCH.schema.fields() {
            let val = match doc.get_first(f) {
                Some(v) => v,
                None => continue,
            };
            match fe.name() {
                "ts" => if let Some(val) = val.as_date() {
                    ts = val.into_timestamp_secs() as u64;
                }
                "title" => {
                    title = val.as_text().map(|s| s.to_string());
                }
                "kind" => if let Some(k) = val.as_text() {
                    if prefered_kind.is_some_and(|pk| pk != k) {
                        continue;   
                    }
                    kind = k.to_string();
                }
                "content" => {
                    content = val.as_text().unwrap().to_string();
                }
                "tags" => {
                    tags = val.as_text().unwrap().to_string();
                }
                "public" => if let Some(pb) = val.as_bool() {
                    public = pb;
                }
                "owner" => if let Some(o) = val.as_u64() {
                    owner = o
                }
                "state" => if let Some(jsn) = val.as_json() {
                    // create a nice json object and stringify it so it is easy to consume on the client side
                    if let Ok(out) = serde_json::to_vec_pretty(&jsn) {
                        if let Ok(s) = String::from_utf8(out) {
                            state = Some(s);
                        }
                    }
                }
                "price" => {
                    price = val.as_u64();
                }
                "sell_price" => {
                    sell_price = val.as_u64();
                }
                _ => {
                    continue;
                }
            }
        }
        if kind.len() == 0 || content.len() == 0 || tags.len() == 0 {
            // remove the document from the index
            let mut index_writer = SEARCH.index_writer.write();
            index_writer.delete_term(Term::from_field_date(
                SEARCH.schema.get_field("ts")?,
                DateTime::from_timestamp_secs(ts as i64)
            ));
            index_writer.commit()?;
            return Err(anyhow!("kind, content, and tags must be set"));
        }
        Ok(Self{ts, kind, owner, public, title, content, state, price, sell_price, tags})
    }

    fn lookup_owner_moniker(&self) -> anyhow::Result<String> {
        Ok(Account::from_id(self.owner)?.moniker)
    }
/*
    #[allow(dead_code)]
    fn is_owner(&self, id: u64) -> bool {
        self.owner == id
    }

    #[allow(dead_code)]
    fn likes(&self, limit: usize) -> anyhow::Result<Vec<u64>> {
        get_liked_by(self.ts, limit)
    }*/

    fn purchase_access(&self, id: u64, gift: Option<u64>) -> anyhow::Result<()> {
        if self.owner == id {
            return Err(anyhow!("you already own this writ"));
        }
        if self.price.is_none() {
            return Err(anyhow!("this writ is not for sale"));
        }
        let mut acc = Account::from_id(id)?;
        if acc.balance < self.price.unwrap() {
            return Err(anyhow!("you don't have enough money to buy this writ"));
        }
        acc.transfer(&mut Account::from_id(self.owner)?, self.price.unwrap())?;
        match gift {
            Some(gift) => {
                if gift == id {
                    return Err(anyhow!("you can't gift yourself"));
                }
                add_access_for(id, &[gift])?
            },
            None => add_access_for(id, &[self.ts])?
        };
        Ok(())
    }

    fn transfer_access(&self, from: u64, to: u64) -> anyhow::Result<()> {
        match check_access_for(from, &[self.ts], None) {
            Ok(()) => transfer_access_for(from, to, &[self.ts]),
            Err(e) => if !e.to_string().contains("no writ access") {
                self.purchase_access(from, Some(to))
            } else {
                Err(e)
            }
        }
    }

    fn add_to_index(&self, index_writer: &mut IndexWriter, schema: &Schema) -> tantivy::Result<()> {
        index_writer.add_document(self.to_doc(schema))?;
        index_writer.commit()?;
        Ok(())
    }

    fn search_for(query: &str, limit: usize, mut page: usize, s: &Search) -> tantivy::Result<Vec<(f32, tantivy::Document)>> {
        if page == 0 { page = 1; }
        let reader = s.index.reader()?;
        let searcher = reader.searcher();
        let schema = s.schema.clone();
        let query_parser = QueryParser::for_index(&s.index, vec![
            schema.get_field("title").unwrap(),
            schema.get_field("content").unwrap(),
            schema.get_field("tags").unwrap()
            // schema.get_field("ts").unwrap(),
            // schema.get_field("owner").unwrap(),
            // schema.get_field("public").unwrap(),
            // schema.get_field("state").unwrap(),
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
            if results.len() >= limit { break; }
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
        schema_builder.add_u64_field("price", FAST | INDEXED | STORED);
        schema_builder.add_u64_field("sell_price", FAST | INDEXED | STORED);
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
        if let Err(e) = add_to_timeline(writ.owner, &[writ.ts]) {
            return Err(
                tantivy::error::TantivyError::SystemError(
                    format!("failed to add writ to timeline: {}", e.to_string())
                )
            );
        }
        writ.add_to_index(&mut index_writer, &self.schema)
    }

    fn get_doc(&self, ts: u64) -> tantivy::Result<Document> {
        if ts == 0 {
            return Err(
                tantivy::error::TantivyError::SystemError(
                    format!("there is no 0")
                )
            );
        }
        let reader = self.index.reader()?;
        let term = Term::from_field_date(self.schema.get_field("ts")?, DateTime::from_timestamp_secs(ts as i64));
        let searcher = reader.searcher();
        let term_query = TermQuery::new(term, IndexRecordOption::Basic);
        let top_docs = searcher.search(&term_query, &TopDocs::with_limit(1))?;
        if top_docs.len() == 0 {
            return Err(
                tantivy::error::TantivyError::SystemError(
                    format!("no such writ")
                )
            );
        }
        let doc_address = top_docs[0].1;
        let doc = searcher.doc(doc_address)?;
        Ok(doc)
    }

    fn remove_doc(&self, owner: u64, ts: u64) -> tantivy::Result<u64> {
        if ts == 0 {
            return Err(
                tantivy::error::TantivyError::SystemError(
                    format!("cannot remove writ with timestamp 0")
                )
            );
        }
        let mut index_writer = self.index_writer.write();
        let op_stamp = index_writer.delete_term(Term::from_field_date(
            self.schema.get_field("ts")?,
            DateTime::from_timestamp_secs(ts as i64)
        ));
        index_writer.commit()?;
        if let Err(e) = rm_from_timeline(owner, &[ts]) {
            return Err(
                tantivy::error::TantivyError::SystemError(
                    format!("failed to remove writ from timeline: {}", e.to_string())
                )
            );
        }
        Ok(op_stamp)
    }

    fn remove_all_docs_by_owner(&self, owner: u64) -> tantivy::Result<u64> {
        let mut index_writer = self.index_writer.write();
        let mut op_stamp = 0;
        let reader = self.index.reader()?;
        let searcher = reader.searcher();
        let term = Term::from_field_u64(self.schema.get_field("owner")?, owner);
        let term_query = TermQuery::new(term, IndexRecordOption::Basic);
        let top_docs = searcher.search(&term_query, &TopDocs::with_limit(100000))?;
        for (_score, doc_address) in top_docs {
            let doc = searcher.doc(doc_address)?;
            let ts = doc.get_first(self.schema.get_field("ts")?).unwrap().as_date().unwrap().into_timestamp_secs() as u64;
            op_stamp = index_writer.delete_term(Term::from_field_date(
                self.schema.get_field("ts")?,
                DateTime::from_timestamp_secs(ts as i64)
            ));
            if let Err(e) = rm_from_timeline(owner, &[ts]) {
                return Err(
                    tantivy::error::TantivyError::SystemError(
                        format!("failed to remove writ from timeline: {}", e.to_string())
                    )
                );
            }
        }
        index_writer.commit()?;
        Ok(op_stamp)
    }

    fn update_doc(&self, writ: &Writ) -> tantivy::Result<()> {
        if writ.ts == 0 {
            return Err(
                tantivy::error::TantivyError::SystemError(
                    format!("0 is not allowed here anymore")
                )
            );
        }
        let mut index_writer = self.index_writer.write();
        index_writer.delete_term(Term::from_field_date(
            self.schema.get_field("ts").unwrap(),
            DateTime::from_timestamp_secs(writ.ts as i64)
        ));
        writ.add_to_index(&mut index_writer, &self.schema)
    }

    fn search(&self, query: &str, limit: usize, page: usize, prefered_kind: Option<&str>, id: Option<u64>) -> anyhow::Result<Vec<Writ>> {
        match Writ::search_for(query, limit, page, self) {
            Ok(results) => {
                let mut writs = vec![];
                let n = now();
                for (_s, d) in results {
                    let mut writ = match Writ::from_doc(&d, prefered_kind) {
                        Ok(writ) => writ,
                        Err(e) => {
                            writs.push(Writ{
                                ts: 0,
                                kind: "error".to_string(),
                                owner: 0,
                                public: false,
                                title: None,
                                content: e.to_string(),
                                state: None,
                                price: None,
                                sell_price: None,
                                tags: String::new()
                            });
                            continue;
                        }
                    };

                    if !writ.public || writ.price.is_some_and(|p| p > 0) {
                        if let Some(id) = id { // check if the owner has access to this writ
                            if writ.owner != id {
                                if let Err(e) = check_access_for(id, &[writ.ts], Some(n)) {
                                    writ.content = e.to_string();
                                }
                            }
                        } else {
                            writ.content = String::new();
                        }
                    }
                    writs.push(writ);
                }
                Ok(writs)
            },
            Err(e) => Err(anyhow!("failed to search for writs: {}", e.to_string())),
        }
    }

    /*#[allow(dead_code)]
    fn build_150mb() -> tantivy::Result<Self> {
        Self::build(150_000_000)
    }*/

    fn build_512mb() -> tantivy::Result<Self> {
        Self::build(512_000_000)
    }
}

pub async fn auth_step(req: &mut Request, res: &mut Response) -> Option<u64> {
    let mut _id: Option<u64> = None; // authenticate
    if let Some(id) = session_check(req, None).await { 
        _id = Some(id); // account session
    } else if let Some(tk) = req.query::<String>("tk") {
        if let Ok((_pm, o, _exp, _uses, _state)) = validate_token_under_permision_schema(&tk, &[u32::MAX, u32::MAX - 1], &DB).await {
            _id = Some(o); // token session
        }
    }
    if _id.is_none() {
        brq(res, "not authorized to use to access this route");
    }
    _id
}

#[handler]
pub async fn writ_access_purchase_gateway_api(req: &mut Request, _depot: &mut Depot, res: &mut Response, _ctrl: &mut FlowCtrl) {
    let id = match auth_step(req, res).await {
        Some(id) => id,
        None => return
    };

    let ts: u64 = match req.param::<u64>("ts") {
        Some(ts) => ts,
        None => {
            brq(res, "no ts param provided");
            return;
        }
    };

    let writ = match SEARCH.get_doc(ts) {
        Ok(doc) => match Writ::from_doc(&doc, None) {
            Ok(writ) => writ,
            Err(e) => {
                brqe(res, &e.to_string(), "failed to get writ");
                return;
            }
        }
        Err(e) => {
            brqe(res, &e.to_string(), "failed to get writ");
            return;
        }
    };

    match req.param::<u64>("to") {
        Some(to_id) => {
            match writ.transfer_access(id, to_id) {
                Ok(_) => jsn(res, serde_json::json!({"ok": true, "msg": "access transfered"})),
                Err(e) => brqe(res, &e.to_string(), "failed to transfer access"),
            }
            return;
        },
        None => {}
    };

    let gift: Option<u64> = match req.query::<u64>("for") {
        Some(gift) => Some(gift),
        None => match req.query::<String>("for") {
            Some(gift) => match Account::from_moniker(&gift) {
                Ok(acc) => Some(acc.id),
                Err(e) => {
                    brqe(res, &e.to_string(), "failed to get account to gift writ access to");
                    return;
                }
            },
            None => None
        }
    };

    match writ.purchase_access(id, gift) {
        Ok(_) => jsn(res, serde_json::json!({"ok": true, "msg": "access purchased"})),
        Err(e) => brqe(res, &e.to_string(), "failed to purchase access"),
    }
}

#[handler]
pub async fn see_writ_reposts(req: &mut Request, _depot: &mut Depot, res: &mut Response, _ctrl: &mut FlowCtrl) {
    if auth_step(req, res).await.is_none() { return; }

    let start = match req.query::<u64>("start") {
        Some(s) => s,
        None => 0
    };
    let count = match req.query::<u64>("count") {
        Some(c) => c,
        None => 1000
    };

    match req.param::<u64>("ts") {
        Some(ts) => match &get_reposters(ts, count, start) {
            Ok(reposters) => {
                let mut monikers = vec![];
                for id in reposters {
                    if let Ok(acc) = Account::from_id(*id) {
                        monikers.push(acc.moniker);
                    } else {
                        monikers.push("unknown".to_string());
                    }
                }
                res.render(Json((reposters, monikers)));
            },
            Err(e) => brqe(res, &e.to_string(), "failed to get reposters")
        },
        None => brq(res, "no ts param provided")
    }
}

#[handler]
pub async fn see_writ_likes(req: &mut Request, _depot: &mut Depot, res: &mut Response, _ctrl: &mut FlowCtrl) {
    if auth_step(req, res).await.is_none() { return; }
    match req.param::<u64>("ts") {
        Some(ts) => match &get_writ_likes(ts, 1000) {
            Ok(likes) => {
                let mut monikers = vec![];
                for id in likes {
                    if let Ok(acc) = Account::from_id(*id) {
                        monikers.push(acc.moniker);
                    } else {
                        monikers.push("unknown".to_string());
                    }
                }
                if likes.len() == 0 {
                    brq(res, "no likes found");
                    return;
                }
                res.render(Json((likes, monikers)));
            },
            Err(e) => brqe(res, &e.to_string(), "failed to get likes"),
        },
        None => brq(res, "no ts param provided")
    }
}

#[derive(Deserialize, Serialize)]
struct SearchRequest{
    query: String,
    kind: Option<String>,
    limit: usize,
    page: usize,
}

#[derive(Deserialize, Serialize)]
struct FoundWrit{
    ts: u64,
    kind: String,
    owner_moniker: String,
    owner: u64,
    public: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    price: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sell_price: Option<u64>,
    tags: String,
    liked: bool,
    reposted: bool,
    repost_count: u64,
    likes: Vec<u64>
}

impl FoundWrit {
    fn build_from_writ(w: &Writ, requester_id: u64) -> anyhow::Result<Self> {
        if !(w.public || w.price.is_some_and(|p| p > 0)) && !(w.owner == requester_id || w.owner == ADMIN_ID || check_access_for(requester_id, &[w.ts], None).is_err()) {
            return Err(anyhow!("you don't have access to this writ"));
        } else {
            tracing::info!("writ({}) is prived/not-public and received priviledged access from acc: ({requester_id})", w.ts);
        }
        let mut liked = false;
        let mut reposted = false;
        let mut repost_count = 0;
        let mut likes = vec![];
        if let Ok(likers) = get_writ_likes(w.ts, 1000) {
            liked = likers.contains(&requester_id);
            likes = likers;
        }
        if let Ok(reposters) = get_reposters(w.ts, 0, 1000) {
            repost_count = reposters.len() as u64;
            reposted = reposters.contains(&requester_id);
        }
        Ok(Self{
            ts: w.ts,
            kind: w.kind.clone(),
            owner_moniker: w.lookup_owner_moniker()?,
            owner: w.owner,
            public: w.public,
            title: w.title.clone(),
            content: w.content.clone(),
            state: w.state.clone(),
            price: w.price,
            sell_price: w.sell_price,
            tags: w.tags.clone(),
            liked,
            reposted,
            repost_count,
            likes,
        })
    }

    fn build_from_writs(writs: Vec<Writ>, requester_id: Option<u64>, null_on_perm_issue: bool) -> anyhow::Result<Vec<Self>> {
        let mut found_writs = vec![];
        let rtx = DB.begin_read()?;
        let t_likes= rtx.open_multimap_table(LIKED_BY)?;
        let t_accounts = rtx.open_table(ACCOUNTS)?;
        let t_reposts = match rtx.open_multimap_table(REPOSTS) {
            Ok(t) => Some(t),
            Err(_e) => None // for if there haven't been reposts yet
        };
        let n: u64 = now();
        // do manual lookup in a single read transaction for likes, owner_monikers, and reposts
        for w in writs {
            let mut nulled = false;
            if !(w.public || w.price.is_none()) && !requester_id.is_some_and(|ri| w.owner == ri || (w.price.is_some_and(|p| p > 0) && check_access_for(ri, &[w.ts], Some(n)).is_ok())) {
                if !null_on_perm_issue {
                    return Err(anyhow!("you don't have access to this writ"));
                }
                nulled = true;
            }
            let mut liked = false;
            let mut reposted = false;
            let mut repost_count = 0;
            let mut likes = vec![];
            let mut owner_moniker = String::new();
            let mut owner = 0;
            
            let mut mmv = t_likes.get(w.ts)?;
            while let Some(r) = mmv.next() {
                let liked_by_id = r?.value();
                if requester_id.is_some_and(|id| id == liked_by_id) {
                    liked = true;
                }
                likes.push(liked_by_id);
            }

            if let Some(t_reposts) = &t_reposts {
                let mmv = t_reposts.get(w.ts)?;
                for r in mmv {
                    let ag  = r?;
                    if requester_id.is_some_and(|id| id == ag.value()) {
                        reposted = true;
                    }
                    repost_count += 1;
                }
            }

            if let Some(ag) = t_accounts.get(w.owner)? {
                owner_moniker = ag.value().0.to_string();
                owner = w.owner;
            }

            found_writs.push(Self{
                ts: w.ts,
                kind: w.kind,
                owner_moniker,
                owner,
                public: w.public,
                title: w.title,
                content: if nulled { String::new() } else { w.content },
                state: w.state,
                price: w.price,
                sell_price: w.sell_price,
                tags: w.tags,
                liked,
                reposted,
                repost_count,
                likes,
            });
        }
        
        Ok(found_writs)
    }
}

#[handler]
pub async fn search_api(req: &mut Request, _depot: &mut Depot, res: &mut Response, _ctrl: &mut FlowCtrl) {
    let mut _is_admin = false;
    let mut _owner: Option<u64> = None; // authenticate
    let is_get = req.method() == Method::GET;
    if let Some(id) = session_check(req, None).await {
        _is_admin = id == ADMIN_ID; // admin session
        _owner = Some(id);
    } else if let Some(tk) = req.query::<String>("tk") {
        if let Ok((_pm, o, _exp, _uses, _state)) = validate_token_under_permision_schema(&tk, &[u32::MAX, u32::MAX - 1], &DB).await {
            _owner = Some(o); // token session
        } else if req.method() != Method::GET {
            brq(res, "not authorized to use the search api");
            return;
        }
    }

    if is_get { // serve public searchable items
        match req.query::<String>("q") {
            Some(q) => {
                let page = match req.query::<usize>("p") {
                    Some(p) => p,
                    None => 0,
                };
                let limit = match req.query::<usize>("l") {
                    Some(l) => match l {
                        0 => 128,
                        l if l > 256 => 256,
                        _ => l,
                    },
                    None => 128,
                };
                let kind = match req.query::<&str>("k") {
                    Some(k) => Some(k),
                    None => None,
                };
                match SEARCH.search(&q, limit, page, kind, _owner) {
                    Ok(writs) => match FoundWrit::build_from_writs(writs, _owner, true) {
                        Ok(mut writs) => {
                            let include_nulled = req.query::<&str>("inc").is_some_and(|n| n.contains("nils"));
                            let exclude_priced = req.query::<&str>("exc").is_some_and(|n| n.contains("priced"));
                            let mut i = 0;
                            if writs.len() == i { return nfr(res); }
                            while i < writs.len() {
                                if writs[i].content.len() == 0 && !(include_nulled || (!exclude_priced && writs[i].price.is_some_and(|p| p > 0))) {
                                    writs.remove(i);
                                } else {
                                    i += 1;
                                }
                            }
                            res.render(Json(writs));
                        },
                        Err(e) => return brqe(res, &e.to_string(), "failed to search")
                    },
                    Err(e) => brqe(res, &e.to_string(), "failed to search"),
                }
            },
            None => {
                if let Some(ts) = req.query::<u64>("ts") { // if there's a ts param serve that one writ
                    match SEARCH.get_doc(ts) {
                        Ok(doc) => match Writ::from_doc(&doc, req.query("k")) {
                            Ok(writ) => match FoundWrit::build_from_writ(&writ, _owner.unwrap()) {
                                Ok(writ) => res.render(Json(writ)),
                                Err(e) => {
                                    brqe(res, &e.to_string(), "failed to get writ");
                                    return;
                                }
                            },
                            Err(e) => {
                                brqe(res, &e.to_string(), "failed to get writ");
                                return;
                            }
                        }
                        Err(e) => {
                            brqe(res, &e.to_string(), "failed to get writ");
                            return;
                        }
                    }
                } else {
                    if let Some(since) = req.query::<u64>("since") { // otherwise if there's a since query param then serve all writs since that timestamp
                        let mut writs = vec![];
                        let reader = SEARCH.index.reader().unwrap();
                        let searcher = reader.searcher();
                        
                        let dt = DateTime::from_timestamp_secs(since as i64);
                        
                        let rq = tantivy::query::RangeQuery::new_date_bounds(
                            "ts".to_string(),
                            std::ops::Bound::Included(dt),
                            std::ops::Bound::Unbounded,
                        );

                        match searcher.search(&rq, &TopDocs::with_limit(2000)) {
                            Ok(top_docs) => {
                                for (_score, doc_address) in top_docs {
                                    let retrieved_doc = match searcher.doc(doc_address) {
                                        Ok(doc) => doc,
                                        Err(e) => {
                                            brqe(res, &e.to_string(), "failed to search");
                                            return;
                                        }
                                    };
                                    match Writ::from_doc(&retrieved_doc, None) {
                                        Ok(writ) => {
                                            let include_nulled = req.query::<&str>("inc").is_some_and(|n| n.contains("nils"));
                                            let exclude_priced = req.query::<&str>("exc").is_some_and(|n| n.contains("priced"));
                                            if include_nulled || (!exclude_priced && writ.price.is_some_and(|p| p > 0)) {
                                                writs.push(writ)
                                            }
                                        },
                                        Err(e) => {
                                            if e.to_string().contains("must be set") {
                                                continue;
                                            } else {
                                                brqe(res, &e.to_string(), "failed to formulate writ postSearch");
                                                return;
                                            }
                                        }
                                    }
                                }
                            },
                            Err(e) => {
                                brqe(res, &e.to_string(), "failed to search");
                                return;
                            }
                        };

                        if writs.len() == 0 {
                            brq(res, "no writs found");
                            return;
                        }

                        match FoundWrit::build_from_writs(writs, _owner, true) {
                            Ok(writs) => res.render(Json(writs)),
                            Err(e) => {
                                brqe(res, &e.to_string(), "failed to search");
                                return;
                            }
                        }
                    } else {
                        brq(res, "no query or ts param provided");
                    }
                }
            }
        }
    } else if req.method() == Method::POST {
        match req.parse_json::<SearchRequest>().await {
            Ok(search_request) => match SEARCH.search(
                &search_request.query,
                search_request.limit,
                search_request.page,
                search_request.kind.as_deref(),
                 _owner
            ) {
                Ok(mut writs) => {
                    let include_nulled = req.query::<&str>("inc").is_some_and(|n| n.contains("nils"));
                    let exclude_priced = req.query::<&str>("exc").is_some_and(|n| n.contains("priced"));
                    for i in 0..writs.len() {
                        if writs[i].content.len() == 0 && !(include_nulled || (!exclude_priced && writs[i].price.is_some_and(|p| p > 0))) {
                            writs.remove(i);
                        }
                    }

                    if writs.len() == 0 {
                        brq(res, "no writs found");
                        return;
                    }
                    match FoundWrit::build_from_writs(writs, _owner, true) {
                        Ok(writs) => res.render(Json(writs)),
                        Err(e) => {
                            brqe(res, &e.to_string(), "failed to search");
                            return;
                        }
                    }
                },
                Err(e) => brqe(res, &e.to_string(), "failed to search"),
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
                    content: if pw.content.len() > 0 {
                        pw.content
                    } else {
                        brq(res, "content is empty, can't update writ");
                        return;
                    },
                    state: pw.state,
                    price: pw.price,
                    sell_price: pw.sell_price,
                    tags: if let Some(tags) = validate_tags_string(pw.tags) {
                        tags
                    } else {
                        brq(res, "invalid tags");
                        return;
                    },
                };

                if writ.ts == 0 {
                    brq(res, "ts cannot be 0");
                    return;
                }

                if writ.ts > now() {
                    brq(res, "ts cannot be in the future");
                    return;
                }

                if writ.ts < now() - (315_360_000 * 2) {
                    brq(res, "ts cannot be more than 20 years in the past");
                    return;
                }

                // ensure writ.kind is valid
                if writ.kind.len() > 256 {
                    brq(res, "kind is too long");
                    return;
                }

                // ensure writ.title is valid
                if let Some(title) = &writ.title {
                    if title.len() > 256 {
                        brq(res, "title is too long");
                        return;
                    }
                }

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
                    tracing::info!("updating writ: {:?}", writ);
                    match SEARCH.update_doc(&writ) {
                        Ok(()) => res.render(Json(serde_json::json!({"ok": true}))),
                        Err(e) => brqe(res, &e.to_string(), "failed to update index"),
                    }
                } else {
                    tracing::info!("adding writ to index: {:?}", writ);
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
                                let owner = _owner.clone().unwrap();
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
                match SEARCH.remove_doc(_owner.unwrap(), ts) {
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
                    content: if pw.content.len() > 0 {
                        pw.content
                    } else {
                        brq(res, "content is empty, can't update writ");
                        return;
                    },
                    state: pw.state,
                    price: pw.price,
                    sell_price: pw.sell_price,
                    tags: if let Some(tags) = validate_tags_string(pw.tags) {
                        tags
                    } else {
                        brq(res, "tags are invalid, can't update writ");
                        return;
                    }
                };

                if writ.ts == 0 {
                    brq(res, "ts cannot be 0");
                    return;
                }

                if writ.ts > now() {
                    brq(res, "ts cannot be in the future");
                    return;
                }

                if writ.ts < now() - (315_360_000 * 2) {
                    brq(res, "ts cannot be more than 20 years in the past");
                    return;
                }

                // ensure writ.kind is valid
                if writ.kind.len() > 64 {
                    brq(res, "kind is too long");
                    return;
                }

                // ensure writ.title is valid
                if let Some(title) = &writ.title {
                    if title.len() > 256 {
                        brq(res, "title is too long");
                        return;
                    }
                }

                if writ.owner != _owner.unwrap() {
                    brq(res, "not authorized to update posts in the index without the right credentials");
                    return;
                }
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

#[handler]
pub async fn timeline_api(req: &mut Request, depot: &mut Depot, res: &mut Response, ctrl: &mut FlowCtrl) {    
    if let Some((owner, pm, _is_admin)) = api_auth_step(req, res).await {
        if pm.is_some() && !pm.is_some_and(|pm| u32::MAX - 1 == pm) {
            brq(res, "not authorized to use the timeline api");
            return;
        }
        let op = match req.param::<&str>("op") {
            Some(op) => op,
            None => {
                brq(res, "no op param provided");
                return;
            }
        };

        match *req.method() {
            Method::GET => {
                let start = match req.query::<u64>("p") {
                    Some(s) => s,
                    None => 0
                };
                let count = match req.query::<u64>("c") {
                    Some(c) => c,
                    None => 512
                };

                match op {
                    "r" | "a" => {
                        let only_reposts = op == "r";
                        match get_timeline(req.query("tl").unwrap_or(owner), start, count) {
                            Ok(writs) => {
                                let pk = req.query("k");
                                let mut found_writs = vec![];
                                for ts in &writs {
                                    match SEARCH.get_doc(*ts) {
                                        Ok(doc) => match Writ::from_doc(&doc, pk) {
                                            Ok(writ) => {
                                                if only_reposts && writ.owner == owner { continue; }
                                                match FoundWrit::build_from_writ(&writ, owner) {
                                                    Ok(writ) => found_writs.push(writ),
                                                    Err(e) => {
                                                        tracing::error!("failed to get writ({}) for {}, err: {}", *ts, owner, e);
                                                    }
                                                }
                                            },
                                            Err(e) => if !(e.to_string().contains("found") || e.to_string().contains("be set")) {
                                                tracing::error!("failed to get writ({}) for {}, err: {}", *ts, owner, e);
                                            } else {
                                                brqe(res, &e.to_string(), "failed to get writ");
                                                return;
                                            }
                                        },
                                        Err(e) => {
                                            if rm_from_timeline(req.query("tl").unwrap_or(owner), &[*ts]).is_ok() {

                                            }
                                            tracing::error!("failed to get writ({}) for {}, err: {}", *ts, owner, e);
                                        }
                                    }
                                }
                                if found_writs.len() == 0 {
                                    brq(res, "no writs found");
                                    return;
                                }
                                jsn(res, found_writs);
                            },
                            Err(e) => brqe(res, &e.to_string(), "failed to get timeline"),
                        }
                    },
                    _ => {
                        match get_ambient_timeline(op.as_bytes(), start, count) {
                            Ok(writs) => {
                                let pk = req.query("k");
                                let mut found_writs = vec![];
                                for ts in &writs {
                                    match SEARCH.get_doc(*ts) {
                                        Ok(doc) => match Writ::from_doc(&doc, pk) {
                                            Ok(writ) => match FoundWrit::build_from_writ(&writ, owner) {
                                                Ok(writ) => found_writs.push(writ),
                                                Err(e) => {
                                                    brqe(res, &e.to_string(), "failed to get writ");
                                                    return;
                                                }
                                            },
                                            Err(e) => {
                                                brqe(res, &e.to_string(), "failed to get writ");
                                                return;
                                            }
                                        },
                                        Err(e) => {
                                            if rm_from_ambient_timeline(op.as_bytes(), &[*ts], None).is_ok() {}
                                            tracing::error!("failed to get writ({}) for {}, err: {}", *ts, owner, e);
                                        }
                                    }
                                }
                                jsn(res, found_writs);
                            },
                            Err(e) => {
                                tracing::info!("failed to get ambient timeline, err: {}", e.to_string());
                                if !ctrl.call_next(req, depot, res).await {
                                    brq(res, "invalid op param provided");
                                }
                            },
                        }
                    }
                }
            },
            Method::POST => {        
                match op {
                    "add" => {
                        let wids = if let Some(wid) = req.query_or_form::<u64>("writ").await {
                            vec![wid]
                        } else if let Some(wids) = req.query_or_form::<Vec<u64>>("writs").await {
                            wids
                        } else {
                            brq(res, "no writ id/s provided");
                            return;
                        };
                        for wid in wids {
                            let d = SEARCH.get_doc(wid);
                            if d.is_err() {
                                brqe(res, &d.err().unwrap().to_string(), "failed to find writ/s, can't repost");
                                return;
                            }
                            if let Ok(writ) = Writ::from_doc(&d.unwrap(), req.query("k")) {
                                if writ.owner != owner {
                                    match repost(owner, &[wid]) {
                                        Ok(_) => jsn(res, serde_json::json!({"ok": true, "msg": "reposted"})),
                                        Err(e) => brqe(res, &e.to_string(), "failed to repost"),
                                    }
                                } else {
                                    match add_to_timeline(owner, &[wid]) {
                                        Ok(_) => jsn(res, serde_json::json!({"ok": true, "msg": "added to timeline"})),
                                        Err(e) => brqe(res, &e.to_string(), "failed to add to timeline"),
                                    }
                                }
                            } else {
                                brq(res, "failed to get writ");
                            }
                        }
                    },
                    "rm" => {
                        let wids = if let Some(wid) = req.query_or_form::<u64>("writ").await {
                            vec![wid]
                        } else if let Some(wids) = req.query_or_form::<Vec<u64>>("writs").await {
                            wids
                        } else {
                            brq(res, "no writ id/s provided");
                            return;
                        };
                        let pk = req.query("k");
                        for wid in wids {
                            let d = SEARCH.get_doc(wid);
                            if d.is_err() {
                                // if the writ doesn't exist in the index then it's already been removed from the timeline
                                continue;
                            }
                            if let Ok(writ) = Writ::from_doc(&d.unwrap(), pk.clone()) {
                                if writ.owner != owner {
                                    match unrepost(owner, &[wid]) {
                                        Ok(_) => jsn(res, serde_json::json!({"ok": true, "msg": "unreposted"})),
                                        Err(e) => brqe(res, &e.to_string(), "failed to unrepost"),
                                    }
                                } else {
                                    match rm_from_timeline(owner, &[wid]) {
                                        Ok(_) => jsn(res, serde_json::json!({"ok": true, "msg": "removed from timeline"})),
                                        Err(e) => brqe(res, &e.to_string(), "failed to remove from timeline"),
                                    }
                                }
                            } else {
                                brq(res, "failed to get writ");
                            }
                        }
                    }
                    _ => {  //if !ctrl.call_next(req, depot, res).await {
                        brq(res, "invalid op param provided");
                    }
                }
            }
            _ => { // if !ctrl.call_next(req, depot, res).await {
                uares(res, "method not allowed");
            }
        }
    } else {
        uares(res, "not authorized to use the timeline api");
        return;
    }
}