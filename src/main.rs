use mimalloc::MiMalloc;
#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

use lazy_static::lazy_static;
use salvo::{conn::rustls::{Keycert, RustlsConfig}, http::{*}, prelude::*, rate_limiter::*, logging::Logger};
use serde::{Serialize, Deserialize};
use serde_json::json;
use sthash::Hasher;
use std::{io::{Read, Write}, fs::File, path::{Path, PathBuf}};
use rand::{thread_rng, distributions::Alphanumeric, Rng};
use redb::{Database, ReadableTable, TableDefinition, MultimapTableDefinition, ReadableMultimapTable};

use base64::{Engine as _};

/*
fn write_string_to_path(path: &str, s: &str) -> std::io::Result<()> {
    write_bytes_to_path(path, s.as_bytes())
}
*/
fn read_string_from_path(path: &str) -> std::io::Result<String> {
    let s = String::from_utf8(read_bytes_from_path(path)?);
    match s {
        Ok(s) => Ok(s),
        Err(e) => Err(std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }
}
fn write_bytes_to_path(path: &str, s: &[u8]) -> std::io::Result<()> {
    let mut file = File::create(path)?;
    file.write_all(s)?;
    Ok(())
}

fn read_bytes_from_path(path: &str) -> std::io::Result<Vec<u8>> {
    let mut file = File::open(path)?;
    let mut s = vec![];
    file.read_to_end(&mut s)?;
    Ok(s)
}

fn get_or_generate_admin_password() -> (Vec<u8>, Hasher) {
    match read_bytes_from_path("./secrets/admin_password.txt") {
        Ok(s) => {
            let phsr = sthash::Hasher::new(sthash::Key::from_seed(s.as_slice(), Some(b"shok")), Some(b"zen secure"));
            (s, phsr)
        },
        Err(_) => {
            let s: String = thread_rng()
                .sample_iter(&Alphanumeric)
                .take(32)
                .map(char::from)
                .collect();
            let phsr = sthash::Hasher::new(sthash::Key::from_seed(s.as_bytes(), Some(b"shok")), Some(b"zen secure"));
            let admin_password_hash = phsr.hash(s.as_bytes());
            // write it to a file
            let mut pwf = File::create("./secrets/ADMIN_PWD.txt").expect("failed to create admin password file");
            pwf.write_all(s.as_bytes()).expect("failed to write admin password to file");
            write_bytes_to_path("./secrets/admin_password.txt", &admin_password_hash).expect("failed to write admin password hash to file");
            (admin_password_hash, phsr)
        }
    }
}

lazy_static!{
    static ref B64: base64::engine::GeneralPurpose = {
        let abc = base64::alphabet::Alphabet::new("+_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789").expect("aplhabet was too much for base64, sorry");
        base64::engine::GeneralPurpose::new(&abc, base64::engine::general_purpose::GeneralPurposeConfig::new().with_encode_padding(false).with_decode_allow_trailing_bits(true))
    };
    static ref PWD: (Vec<u8>, Hasher) = get_or_generate_admin_password();
}

fn check_admin_password(pwd: &[u8]) -> bool {
    &PWD.1.hash(pwd) == PWD.0.as_slice()
}

const ADMIN_ID: u64 = 1997;
const STATIC_DIR: &'static str = "./static/";

const SESSIONS: TableDefinition<&str, (u64, u64)> = TableDefinition::new("sessions");
// Accounts             name, since, xp, balance, pwd_hash
const ACCOUNTS: TableDefinition<u64, (&str, u64, u64, u64, &[u8])> = TableDefinition::new("accounts");
const ACCOUNT_MONIKER_LOOKUP: TableDefinition<&str, u64> = TableDefinition::new("account_moniker_lookup");
// Tokens                            perm_schema, account_id, expiry, uses, state
const TOKENS: TableDefinition<&[u8], (u32, u64, u64, u64, Option<&[u8]>)> = TableDefinition::new("tokens");

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
    categories: Vec<String>,
    tags: Vec<String>,
    kind: Option<String>,
    meta_data: Option<Vec<u8>>,
    mime_type: Option<String>,
    owners: HashMap<u64, u64>, // account_id, shares
    sellable: bool,
    price: u64,
    coupons: HashMap<String, (u64, bool, Option<Vec<u8>>)>, // code, (discount, once, more_data)
    expiry: u64,
    description: Option<String>,
    state: Option<Vec<u8>>
}
 */





/*
    Permision Schemas should be determinative of the state deserialization type of token's Option<&[u8]> state field
    0: read
    1: read/write
    2: read/write/execute
    3: read/write/execute/transact

*/

#[allow(dead_code)]
const STAKED_TRANSFER_CONTRACTS: MultimapTableDefinition<
    (u64, u64), // from, to
    (
        u64, // kind
        u64, // amount
        u64, // expiry
        (u64, &[u8]), // (when, &state)
        u64, // return-to
        &[u8] // state
    )
> = MultimapTableDefinition::new("staked_transfer_contracts");

const PERM_SCHEMAS : MultimapTableDefinition<u32, &str> = MultimapTableDefinition::new("perm_schemas");

#[derive(Serialize, Deserialize)]
struct PermSchema(u32, Vec<String>);

impl PermSchema {
    fn ensure_basic_defaults(db: &Database) {
        let wrtx = db.begin_write().expect("db write failed");
        {
            let mut t = wrtx.open_multimap_table(PERM_SCHEMAS).expect("db write failed");
            t.insert(0, "r").expect("db read permision default insert failed");
            t.insert(1, "rw").expect("db read/write permision default insert failed");
            t.insert(2, "rwx").expect("db read/write/execute permision default insert failed");
            t.insert(3, "t").expect("db transact default insert failed");
        }
        wrtx.commit().expect("failed to insert basic default perm schemas");
    }

    fn modify(add: Option<Vec<String>>, rm: Option<Vec<String>>, id: Option<u32>, db: &Database) -> Result<Self, redb::Error> {
        let wrtx = db.begin_write()?;
        let id = match id {
            Some(i) => i,
            None => {
                // random
                let mut rng = thread_rng();
                loop {
                    let i = rng.gen::<u32>();
                    if i > 3 {
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
    fn add_perms(pm: u32, db: &Database, perms: &[&str]) -> Result<(), redb::Error> {
        let wrtx = db.begin_write()?;
        {
            let mut t = wrtx.open_multimap_table(PERM_SCHEMAS)?;
            for p in perms.iter() {
                t.insert(pm, p)?;
            }
        }
        wrtx.commit()?;
        Ok(())
    }
    #[allow(dead_code)]
    fn remove_perms(pm: u32, db: &Database, perms: &[&str]) -> Result<(), redb::Error> {
        let wrtx = db.begin_write()?;
        {
            let mut t = wrtx.open_multimap_table(PERM_SCHEMAS)?;
            for p in perms.iter() {
                t.remove(pm, p)?;
            }
            // TODO: cleanup perm schema if it is empty
        }
        wrtx.commit()?;
        Ok(())
    }
}


async fn make_tokens(pm: u32, id: u64, mut count: u16, exp: u64, uses: u64, data: Option<&[u8]>, db: &Database) -> Result<Vec<String>, redb::Error> {
    let mut tkns = vec![];
    let mut tk: String;
    let wtx = db.begin_write()?;
    {
        let mut t = wtx.open_table(TOKENS)?;
        while count > 0 {
            tk = thread_rng().sample_iter(&Alphanumeric).take(32).map(char::from).collect();
            let token_hash = TOKEN_HASHER.hash(tk.as_bytes());
            t.insert(token_hash.as_slice(), (pm, id, exp, uses, data))?;
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

fn read_token(tkh: &[u8], db: &Database) -> Result<(u32, u64, u64, u64, Option<Vec<u8>>), redb::Error> {
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

async fn validate_token_under_permision_schema(tk: &str, pms: &[u32], db: &Database) -> Result<(u32, u64, u64, u64, Option<Vec<u8>>), redb::Error> {
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
            if let Ok((pm, id, exp, _, state)) = validate_token_under_permision_schema(tk, &[], &DB).await { // (u32, u64, u64, u64, Option<Vec<u8>>)
                match action {
                    "/create-resource" => if req.method() == Method::POST && [1].contains(&pm) && state.is_some() {
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
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt().init();
    //let db = init_surrealdb_connection().await?;
    
    let sp = PathBuf::new().join(STATIC_DIR);
    println!("Static files dir: exists - {:?}, {}", sp.exists(), sp.into_os_string().into_string().unwrap_or("bad path".to_string()));
    
    PermSchema::ensure_basic_defaults(&DB);

    let addr = ("0.0.0.0", 8000);
    let config = load_config();

    let limiter = RateLimiter::new(
        FixedGuard::new(),
        MemoryStore::new(),
        RemoteIpIssuer,
        BasicQuota::per_second(4),
    );

    let router = Router::with_hoop(Logger::new()).hoop(CachingHeaders::new()).hoop(limiter)
        .push(
            Router::with_path("/healthcheck")
                .get(health)
        )
        .push(
            Router::with_path("/api")
                .push(
                    Router::with_path("/auth")
                    .post(auth_handler)
                )
                .push(
                    Router::with_path("/cmd")
                        .post(cmd_request)
                )
                .push(
                    Router::with_path("/speak")
                        .post(speak)
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
                    Router::with_path("/resource")
                    .post(resource_api)
                    .path("/<hash>")
                    .handle(resource_api)
                )
                .push(
                    Router::with_path("/<action>/<tk>")
                    .handle(action_token_handler)
                )
        )
        .push(Router::with_path("/paka/<**rest>").handle(Proxy::new(["http://localhost:9797/"])))
        .push(
            Router::with_hoop(Compression::new().enable_gzip(CompressionLevel::Minsize))
                .path("/<file>")
                .get(static_file_route_rewriter)
        )
        .push(
            Router::with_hoop(Compression::new().enable_gzip(CompressionLevel::Minsize))
                .path("<**path>")
                .get(
                    StaticDir::new(STATIC_DIR)
                        .defaults("index.html")
                        .listing(true)
                )
        );

    let listener = TcpListener::new(addr.clone()).rustls(config.clone());

    let acceptor = QuinnListener::new(config, addr)
        .join(listener)
        .bind().await;
    
    Server::new(acceptor).serve(router).await;
}


#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Account {
    id: u64, 
    moniker: String,
    since: u64,
    xp: u64,
    balance: u64,
    pwd_hash: Vec<u8>,
}

impl Account {
    pub fn new(id: u64, moniker: String, pwd_hash: Vec<u8>) -> Self {
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
        let mut sid = None;
        let rtx = db.begin_read()?;
        let t = rtx.open_table(ACCOUNT_MONIKER_LOOKUP)?;
        if let Some(ag) = t.get(moniker)? {
            sid = Some(ag.value());
        }
        if !sid.is_some() {
            return Err(redb::Error::Io(std::io::Error::new(std::io::ErrorKind::Other, "Account not found.")));
        }
        let id = sid.unwrap();
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
        Ok(acc.unwrap())
    }

    pub fn check_password(&self, pwd: &[u8]) -> bool {
        PWD.1.hash(pwd) == self.pwd_hash
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
        {
            let mut t = wrtx.open_table(ACCOUNTS)?;
            if new_acc {
                // prevent clash see if there's a match already for moniker and id, if so, return error
                if let Some(ag) = t.get(self.id)? {
                    if ag.value().0 == self.moniker {
                        return Err(redb::Error::Io(std::io::Error::new(std::io::ErrorKind::Other, "Account already exists.")));
                    }
                }
            }
            t.insert(self.id, (self.moniker.as_str(), self.since, self.xp, self.balance, self.pwd_hash.as_slice()))?;
        }
        {
            let mut t = wrtx.open_table(ACCOUNT_MONIKER_LOOKUP)?;
            // prevent clash see if there's a match already for moniker if so, return error
            if let Some(_ag) = t.get(self.moniker.as_str())? {
                return Err(redb::Error::Io(std::io::Error::new(std::io::ErrorKind::Other, "Unauthorized.")));
                // let id = ag.value(); if id != self.id {}
            }
            t.insert(self.moniker.as_str(), self.id)?;
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

#[derive(Serialize, Deserialize)]
pub struct Session(String, u64, u64); // auth cookie, id, expiry

impl Session {
    pub fn new(id: u64, token: Option<String>) -> Self {
        Self(
            token.unwrap_or_else(|| format!("{}-{}", id, thread_rng()
                .sample_iter(Alphanumeric)
                .take(32)
                .map(char::from)
                .collect::<String>()
            )),
            id,
            now() + (60 * 60 * 24 * 7)
        )
    }

    pub fn save(&self, db: &Database) -> Result<(), redb::Error> {
        let wrtx = db.begin_write()?;
        {
            let mut t = wrtx.open_table(SESSIONS)?;
            t.insert(self.0.as_str(), (self.1, self.2))?;
        }
        wrtx.commit()?;
        Ok(())
    }

    pub fn expired(&self) -> bool {
        self.2 < now()
    }

    pub fn check(auth: &str, db: &Database) -> Result<Self, redb::Error> {
        let wrtx = db.begin_write()?;
        let mut expired = false;
        let mut exiry_timestamp = 0;
        let mut sid = None;
        {
            let mut t = wrtx.open_table(SESSIONS)?;
            if let Some(ag) = t.get(auth)? {
                let (id, exp) = ag.value();
                expired = exp < now();
                sid = Some(id);
                exiry_timestamp = exp;
            }
            if expired {
                t.remove(auth)?;
                return Err(redb::Error::Io(std::io::Error::new(std::io::ErrorKind::Other, "Session expired.")));
            }
        }
        wrtx.commit()?;
        if !sid.is_some() {
            return Err(redb::Error::Io(std::io::Error::new(std::io::ErrorKind::Other, "Session not found.")));
        }
        return Ok(Self(auth.to_string(), sid.unwrap(), exiry_timestamp));
    }
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
}

fn read_to_byte_vec(path: &str) -> Vec<u8> {
    let mut file = std::fs::File::open(path.clone()).expect(&format!("Failed to open file at {:?}.", path));
    let mut buf = Vec::new();
    file.read_to_end(&mut buf).expect("Failed to read file.");
    buf
}

fn load_config() -> RustlsConfig {
    RustlsConfig::new(Keycert::new()
        .cert(read_to_byte_vec("./secrets/cert.pem"))
        .key(read_to_byte_vec("./secrets/priv.pem"))
    )
}

#[handler]
async fn static_file_route_rewriter(req: &mut Request, depot: &mut Depot, res: &mut Response, ctrl: &mut FlowCtrl) {
    match req.param::<String>("file") {
        Some(file) => {
            // check if a version of file + ".html" exists first and if it does, serve that
            let mut path = PathBuf::new().join(STATIC_DIR).join(file.clone());
            if path.extension().is_none() {
                path.set_extension("html");
                if path.exists() {
                    StaticFile::new(path).handle(req, depot, res, ctrl).await;
                    return;
                }
            }
            if path.exists() {
                StaticFile::new(&format!("{}{}", STATIC_DIR, file)).handle(req, depot, res, ctrl).await;
            } else {
                path = PathBuf::new().join("./uploaded/").join(file.clone());
                if path.exists() {
                    // query param token 
                    match req.query::<String>("tk") {
                        Some(tk) => {
                            // check if token is valid
                            if !validate_token_under_permision_schema(&tk, &[0, 1], &DB).await.is_ok() {
                                brq(res, "invalid token");
                                return;
                            }
                        },
                        None => {
                            if !session_check(req, Some(ADMIN_ID)).await {
                                brq(res, "unauthorized");
                                return;
                            }
                        }
                    }

                    StaticFile::new(path).handle(req, depot, res, ctrl).await;
                    return;
                }
            }
        },
        None => {
            if !ctrl.call_next(req, depot, res).await {
                res.render(StatusError::bad_request().brief("missing file param, or invalid url"));
            }
        }
    }
}

#[handler]
async fn upsert_static_file(req: &mut Request, _depot: &mut Depot, res: &mut Response, _ctrl: &mut FlowCtrl) {
    // query param token 
    match req.param::<String>("tk") {
        Some(tk) => {
            // check if token is valid
            if !validate_token_under_permision_schema(&tk, &[1], &DB).await.is_ok() {
                brq(res, "invalid token");
                return;
            }
        },
        None => {
            if !session_check(req, Some(ADMIN_ID)).await {
                brq(res, "unauthorized");
                return;
            }
        }
    }

    let file = req.file("file").await;
    if let Some(file) = file {
        let dest = format!("./uploaded/{}", &file.name().map(|f| f.to_string()).unwrap_or_else(|| format!("file-{}", thread_rng()
            .sample_iter(Alphanumeric)
            .take(16)
            .map(char::from)
            .collect::<String>())));
        let info = if let Err(e) = std::fs::copy(&file.path(), Path::new(&dest)) {
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

async fn auth_check(pwd: &str) -> bool {
    check_admin_password(pwd.as_bytes())
}

async fn session_check(req: &mut Request, id: Option<u64>) -> bool {
    if let Some(session) = req.cookie("auth") {
        if let Ok(s) = Session::check(session.value(), &DB) {
            if let Some(id) = id {
                return s.1 == id;
            }
            return true;
        }   
    }
    false
}


#[derive(Debug, Serialize, Deserialize)]
struct AuthRequest {
    moniker: String,
    pwd: String
}

#[handler]
async fn auth_handler(req: &mut Request, depot: &mut Depot, res: &mut Response, ctrl: &mut FlowCtrl) {
    // get a string from the post body, and set it as a variable called text
    if let Ok(ar) = req.parse_json_with_max_size::<AuthRequest>(8400).await {
        if let Ok(acc) = Account::from_moniker(&ar.moniker, &DB) {
            if !acc.check_password(ar.pwd.as_bytes()) {
                res.status_code(StatusCode::UNAUTHORIZED);
                res.render(Json(serde_json::json!({"err":"bad password"})));
                return;
            }
        }
        // random session token
        let mut session_token = rand::thread_rng()
            .sample_iter(&rand::distributions::Alphanumeric)
            .take(32)
            .map(char::from)
            .collect::<String>();

        if ar.moniker == "admin" {
            let mut new_admin = false;
            // check if admin exists
            let acc = match Account::from_id(ADMIN_ID, &DB) {
                Ok(acc) => if acc.check_password(ar.pwd.as_bytes()) {
                    acc
                } else {
                    brq(res, "admin password incorrect");
                    return;
                },
                Err(_) => {
                    new_admin = true;
                    let pwd_hash = PWD.1.hash(ar.pwd.as_bytes());
                    Account::new(ADMIN_ID, ar.moniker.clone(), pwd_hash)
                }    
            };

            if acc.save(&DB, new_admin).is_ok() {
                session_token = format!("{}-{}", session_token, ADMIN_ID);
                if Session::new(ADMIN_ID, Some(session_token.clone())).save(&DB).is_ok() {
                    res.status_code(StatusCode::ACCEPTED);
                    res.add_cookie(cookie::Cookie::new("auth", session_token.clone()));
                    res.render(Json(serde_json::json!({
                        "msg": "authorized, remember to save the admin password, it will not be shown again",
                        "sid": ADMIN_ID,
                        "pwd": read_string_from_path("./ADMIN_PASSWORD.txt").expect("failed to read admin password")
                    })));
                } else {
                    brq(res, "failed to save session, try another time or way");
                }
                return;
            }
        }
        
        let sid: u64 = {
            let mut sid = rand::thread_rng().gen::<u64>();
            // check for clash
            while Account::from_id(sid, &DB).is_ok() {
                sid = rand::thread_rng().gen::<u64>();
            }
            sid
        };

        session_token = format!("{}-{}", session_token, sid);

        let pwd_hash = PWD.1.hash(ar.pwd.as_bytes());
        let acc = Account::new(sid, ar.moniker.clone(), pwd_hash);

        match acc.save(&DB, true) {
            Ok(()) => {
                if Session::new(sid, Some(session_token.clone())).save(&DB).is_ok() {
                    res.status_code(StatusCode::ACCEPTED);
                    res.add_cookie(cookie::Cookie::new("auth", session_token.clone()));
                    res.render(Json(serde_json::json!({
                        "msg": "authorized",
                        "sid": sid
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
        brq(res, "failed to authorize, bad AuthRequest details");
    }
}

fn zstd_compress(data: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut encoder = zstd::stream::Encoder::new(Vec::new(), 0)?;
    encoder.write_all(data)?;
    Ok(encoder.finish()?)
}

fn zstd_decompress(data: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut decoder = zstd::stream::Decoder::new(std::io::Cursor::new(data))?;
    let mut buf = Vec::new();
    decoder.read_to_end(&mut buf)?;
    Ok(buf)
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct ResourcePostRequest {
    old_hash: Option<Vec<u8>>,
    public: Option<bool>,
    until: Option<u64>,
    mime: String,
    version: Option<u64>,
    data: Vec<u8>
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Resource {
    hash: Vec<u8>,
    owner: Option<u64>,
    since: u64,
    public: bool,
    until: Option<u64>,
    size: usize,
    mime: String,
    reads: u64,
    writes: u64,
    data: Option<Vec<u8>>,
    version: u64
}

#[allow(dead_code)]
impl Resource {
    fn new(hash: Vec<u8>, owner: Option<u64>, size: usize, mime: String) -> Self {
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

    pub fn with_data(mut self, data: &[u8]) -> Self {
        self.data = Some(data.to_vec());
        self
    }

    fn hash(data: &[u8]) -> Vec<u8> {
        RESOURCE_HASHER.hash(data)
    }

    pub fn save(&mut self, bump: bool) -> std::io::Result<()> {
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
        let write_result = file.write_all(&zstd_compress(&serde_json::to_vec(self)?)?);
        if write_result.is_ok() {
            let data_file_write_result = data_file.write_all(&self.data()?);
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

    pub fn set_data(&mut self, data: &[u8]) -> std::io::Result<()> {
        self.data = Some(zstd_compress(data)?);
        self.size = data.len();
        self.save(true)
    }

    pub fn data(&mut self) -> std::io::Result<Vec<u8>> {
        if self.data.is_some() {
            return Ok(self.data.clone().unwrap());
        }
        let path = format!("./assets/{}", B64.encode(&self.hash));
        let mut file = File::open(path)?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        self.set_data(&bytes)?;
        Ok(zstd_decompress(&bytes)?)
    }

    pub fn from_hash(hash: &[u8], with_data: bool) -> std::io::Result<Self> {
        let path = format!("./assets-state/{}", B64.encode(hash));
        let mut file = std::fs::File::open(path)?;
        let mut bytes = vec![];
        file.read_to_end(&mut bytes)?;
        std::mem::forget(file);
        let mut r: Resource = serde_json::from_slice(&zstd_decompress(&bytes)?)?;
        r.reads += 1;
        r.save(false)?;
        if with_data {
            r.data()?;
        }
        Ok(r)
    }
    
    pub fn from_b64_straight(hash: &str, with_data: bool) -> std::io::Result<Self> {
        let path = format!("./assets-state/{}", hash);
        let mut file = std::fs::File::open(path)?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        let mut r: Resource = serde_json::from_slice(&zstd_decompress(&bytes)?)?;
        r.reads += 1;
        r.save(false)?;
        if with_data {
            r.data()?;
        }
        Ok(r)
    }
}


#[handler]
async fn resource_api(req: &mut Request, _depot: &mut Depot, res: &mut Response, _ctrl: &mut FlowCtrl) {
    let mut _pm: Option<u32> = None;
    let mut _owner: Option<u64> = None;
    let mut _is_admin = false;
    if session_check(req, Some(ADMIN_ID)).await {
        // admin session
        _is_admin = true;
    } else {
        if let Some(tk) = req.query::<String>("tk") {
            if let Ok((perm_schema, owner, _, _, _)) = validate_token_under_permision_schema(&tk, &[0, 1], &DB).await {
                // token session
                _pm = Some(perm_schema);
                _owner = Some(owner);
            } else {
                brq(res, "not authorized to make tokens");
                return;
            }
        }
        brq(res, "not authorized to make tokens");
        return;
    }

    match *req.method() {
        Method::GET => match req.param::<String>("hash") {
            Some(hash) => if _is_admin || _pm.is_some_and(|pm| [0, 1].contains(&pm)) {
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
        Method::POST if _is_admin || _pm.is_some_and(|pm| [1].contains(&pm)) => {
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
        Method::DELETE if _is_admin || _pm.is_some_and(|pm| [1].contains(&pm)) => match req.param::<String>("hash") {
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

fn brqe(res: &mut Response, err: &str, msg: &str) {
    res.status_code(StatusCode::BAD_REQUEST);
    res.render(Json(serde_json::json!({
        "msg": msg,
        "err": err
    })));
}

#[derive(Debug, Serialize, Deserialize)]
struct MakeTokenRequest {
    id: u64,
    count: u16,
    uses: Option<u64>,
    state: Option<Vec<u8>>,
    pm: u32,
    exp: Option<u64>
}

#[handler]
async fn make_token_request(req: &mut Request, _depot: &mut Depot, res: &mut Response, _ctrl: &mut FlowCtrl) {
    // get a string from the post body, and set it as a variable called text
    if let Ok(mtr) = req.parse_json_with_max_size::<MakeTokenRequest>(2048).await {
        if session_check(req, Some(ADMIN_ID)).await {
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
struct PermSchemaCreationRequest{
    pwd: Option<String>,
    id: Option<u32>,
    add: Option<Vec<String>>,
    rm: Option<Vec<String>>
}

#[handler]
async fn modify_perm_schema(req: &mut Request, _depot: &mut Depot, res: &mut Response, _ctrl: &mut FlowCtrl) {
    // get a string from the post body, and set it as a variable called text
    if let Ok(pm) = req.parse_json_with_max_size::<PermSchemaCreationRequest>(1024).await {
        if pm.pwd.is_some() || !auth_check(&pm.pwd.unwrap()).await {
            if session_check(req, Some(ADMIN_ID)).await {
                // admin session
            } else {
                res.status_code(StatusCode::BAD_REQUEST);
                res.render(Text::Plain("bad password and/or invalid session"));
                return;
            }
        }

        match PermSchema::modify(pm.add, pm.rm, pm.id, &DB) {
            Ok(ps) => jsn(res, ps),
            Err(e) => brqe(res, &e.to_string(), "failed to save perm schema")
        }
    } else {
        brq(res, "failed to setup perm schema, bad PermSchema details");
    }
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
        if sr.pwd.is_none() || !auth_check(&sr.pwd.clone().unwrap()).await {
            if session_check(req, Some(ADMIN_ID)).await {
                // admin session
            } else {
                res.status_code(StatusCode::BAD_REQUEST);
                res.render(Text::Plain("bad password"));
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


#[derive(Debug, Serialize, Deserialize)]
struct CMDRequest{
    cmd: String,
    args: Vec<String>,
}

impl CMDRequest {
    async fn run(&self) -> std::io::Result<std::process::Output> {
        let mut cmd = std::process::Command::new(&self.cmd);
        for arg in &self.args {
            cmd.arg(arg);
        }
        cmd.output()
    }
}


#[handler]
async fn cmd_request(req: &mut Request, _depot: &mut Depot, res: &mut Response) {
    if !session_check(req, Some(ADMIN_ID)).await {
        // unauthorized
        res.status_code(StatusCode::UNAUTHORIZED);
        res.render(Json(serde_json::json!({"err": "unauthorized"})));
        return;
    }
    // get a string from the post body, and set it as a variable called text
    if let Ok(sr) = req.parse_json_with_max_size::<CMDRequest>(168192).await {
        match sr.run().await {
            Ok(result) => {
                let output = String::from_utf8_lossy(&result.stdout);
                let err = String::from_utf8_lossy(&result.stderr);
                res.render(Json(serde_json::json!({
                    "output": output,
                    "err": err,
                })));
            },
            Err(e) => brqe(res, &e.to_string(), "failed to run command, bad command prolly")
        }
    } else {
        brq(res, "failed to run command, bad CMDRequest");
    }
}