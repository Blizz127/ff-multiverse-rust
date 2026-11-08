//! Saved account tokens: where the token of a registered account lives on this machine.
//!
//! A registration is answered with the account's token exactly once (the server keeps only a
//! hash), and the display name is unique, so a lost token means a lost name. Everything here is
//! therefore careful in three ways:
//!
//! * **Private.** `<state dir>/accounts/` is created with mode 0700 and every token file with mode
//!   0600 (unix). A token file that is group- or other-readable is reported by [`find`] so the
//!   caller can warn once.
//! * **Never half-written.** A file is written under a temporary name, fsynced, and only then
//!   published: with `link(2)` for a NEW file (it fails if the name exists, so two processes can
//!   never both create it, and a reader can never see a partial file) or `rename(2)` to REPLACE
//!   one. The directory is fsynced afterwards. If anything fails the temporary file is removed and
//!   no token file appears.
//! * **Never someone else's account.** Several accounts can live on one server (two bots sharing a
//!   state dir, say). Each lookup that names an account checks the NAME stored in the file, not
//!   just the file name, so two names that happen to map to the same file name can never be
//!   confused with each other.
//!
//! # Files
//!
//! * `<server-key>.token` is the default account of a server; `<server-key>.<name-key>.token` an
//!   additional one. See [`server_key`] and [`name_key`].
//! * Format (text, two lines): the token, then `name=<display name>`. At most 1 KiB; the token is
//!   1 to 256 visible ASCII characters (no whitespace). A file that does not parse is unusable and
//!   is reported, never guessed at.
//!
//! No error text produced here ever contains a token.
//!
//! [`find`]: TokenStore::find

use crate::net::clean;
use fmv_protocol::Secret;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Directory (inside the client state directory) that holds the token files.
pub const ACCOUNTS_DIR: &str = "accounts";
/// File name suffix of every token file.
const TOKEN_SUFFIX: &str = ".token";
/// A token file larger than this is not ours (a token file is about 40 bytes).
pub const MAX_FILE_BYTES: u64 = 1024;
/// Longest token accepted, in bytes.
pub const MAX_TOKEN_BYTES: usize = 256;
/// Longest stored display name, in bytes (the server's limit is 24 characters).
const MAX_NAME_BYTES: usize = 256;
/// Longest server key / name key, in characters.
const SERVER_KEY_MAX: usize = 80;
const NAME_KEY_MAX: usize = 40;

/// Lowercase `text`, replace every character outside `[a-z0-9.-]` with `_` and cap the length.
/// Never empty (a key becomes part of a file name).
fn key_of(text: &str, max: usize) -> String {
    let mut key: String = text
        .to_lowercase()
        .chars()
        .map(|c| if matches!(c, 'a'..='z' | '0'..='9' | '.' | '-') { c } else { '_' })
        .take(max)
        .collect();
    if key.is_empty() {
        key.push('_');
    }
    key
}

/// The file-name key of a server: the normalised `--server` string, lowercased, every character
/// outside `[a-z0-9.-]` replaced by `_`, at most 80 characters. `127.0.0.1:47770` becomes
/// `127.0.0.1_47770`; the brackets and colons of an IPv6 address become `_` as well.
pub fn server_key(server: &str) -> String {
    key_of(server, SERVER_KEY_MAX)
}

/// The file-name key of a display name: lowercased, every character outside `[a-z0-9.-]` replaced
/// by `_`, at most 40 characters. Different names can share a key (`Red XIII` and `red_xiii`);
/// lookups therefore also compare the name stored in the file.
pub fn name_key(name: &str) -> String {
    key_of(name, NAME_KEY_MAX)
}

/// Display names are unique case-insensitively on the server, so they are compared that way here.
fn names_equal(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

/// Why a token cannot be stored or was read from a file, as text that never contains the token.
fn check_token(token: &str) -> Result<(), &'static str> {
    if token.is_empty() {
        Err("the token is empty")
    } else if token.len() > MAX_TOKEN_BYTES {
        Err("the token is longer than 256 bytes")
    } else if !token.bytes().all(|b| (0x21..=0x7e).contains(&b)) {
        Err("the token must be visible ASCII characters without spaces")
    } else {
        Ok(())
    }
}

fn check_name(name: &str) -> Result<(), &'static str> {
    if name.is_empty() {
        Err("the display name is empty")
    } else if name.len() > MAX_NAME_BYTES {
        Err("the display name is too long")
    } else if name.chars().any(char::is_control) {
        Err("the display name contains a control character")
    } else {
        Ok(())
    }
}

/// The text of a token file for `token` and `name`: two lines, `token` then `name=<name>`. `Err`
/// (a reason without the token) when either cannot be stored in the format.
pub fn format_token_file(token: &Secret, name: &str) -> Result<String, String> {
    check_token(token.expose()).map_err(str::to_string)?;
    check_name(name).map_err(str::to_string)?;
    Ok(format!("{}\nname={name}\n", token.expose()))
}

/// Parse the text of a token file (see the module docs). The error says what is wrong with the
/// file and never repeats its content.
pub fn parse_token_file(text: &str) -> Result<(Secret, String), String> {
    if text.len() as u64 > MAX_FILE_BYTES {
        return Err("the file is larger than 1 KiB".into());
    }
    if text.trim().is_empty() {
        return Err("the file is empty".into());
    }
    // Lines end with `\n` or `\r\n`; the final newline is optional.
    let mut lines = text.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l));
    let token = lines.next().unwrap_or("");
    check_token(token).map_err(|why| format!("line 1: {why}"))?;
    let name_line = lines.next().ok_or("the `name=` line is missing")?;
    let rest: Vec<&str> = lines.collect();
    if !(rest.is_empty() || (rest.len() == 1 && rest[0].is_empty())) {
        return Err("unexpected extra lines after the `name=` line".into());
    }
    let name = name_line.strip_prefix("name=").ok_or("line 2 is not a `name=<display name>` line")?;
    check_name(name).map_err(|why| format!("line 2: {why}"))?;
    Ok((Secret::new(token), name.to_string()))
}

/// A saved account.
#[derive(Debug, Clone)]
pub struct SavedAccount {
    /// The token (a [`Secret`]: it prints as `Secret(<redacted>)`).
    pub token: Secret,
    /// The display name the account was registered with.
    pub name: String,
    /// The file it came from.
    pub path: PathBuf,
    /// The file's permission bits (unix) if group or others can read it, else `None`. A token is
    /// a password: the caller warns about a file that returns `Some`.
    pub loose_mode: Option<u32>,
}

/// The one-line warning for a token file that is readable by other users.
pub fn loose_mode_warning(path: &Path, mode: u32) -> String {
    format!(
        "warning: the token file {} is readable by other users (mode {:03o}); anyone who can read it can log in as you. Fix it with: chmod 600 '{}'",
        path.display(),
        mode & 0o777,
        path.display()
    )
}

/// Why a saved account could not be found, read or written.
#[derive(Debug, thiserror::Error)]
pub enum AccountsError {
    /// Several accounts are saved for the server and none was asked for.
    #[error("several accounts are saved for this server ({names}): pass --name to choose one")]
    Ambiguous { names: String },
    /// A token file exists but cannot be used (reported, never guessed at).
    #[error("the saved account file {} is unusable: {reason} (fix or delete it)", .path.display())]
    Unusable { path: PathBuf, reason: String },
    /// Both file names a new account could use are taken (by other accounts).
    #[error("cannot save the token: {} already exists and belongs to another account", .path.display())]
    Exists { path: PathBuf },
    /// The token or name cannot be stored in the file format.
    #[error("cannot save the account: {0}")]
    Invalid(String),
    /// A saved login was not removed (see [`TokenStore::forget`]).
    #[error("cannot forget the saved login: {0}")]
    Forget(String),
    #[error("{what} {}: {source}", .path.display())]
    Io { what: &'static str, path: PathBuf, source: io::Error },
}

fn io_err(what: &'static str, path: &Path, source: io::Error) -> AccountsError {
    AccountsError::Io { what, path: path.to_path_buf(), source }
}

#[cfg(unix)]
fn loose_mode_of(meta: &fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    let mode = meta.permissions().mode() & 0o777;
    (mode & 0o077 != 0).then_some(mode)
}

#[cfg(not(unix))]
fn loose_mode_of(_meta: &fs::Metadata) -> Option<u32> {
    None
}

/// Read and parse one token file. `Ok(None)` if it does not exist; an unusable file is an error.
fn read_account(path: &Path) -> Result<Option<SavedAccount>, AccountsError> {
    let unusable = |reason: &str| AccountsError::Unusable { path: path.to_path_buf(), reason: reason.to_string() };
    let meta = match fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io_err("cannot read", path, e)),
    };
    // Checked before opening: opening a FIFO would block forever.
    if !meta.is_file() {
        return Err(unusable("it is not a regular file"));
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err(unusable("it is larger than 1 KiB"));
    }
    let mut bytes = Vec::new();
    match File::open(path) {
        Ok(f) => f.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes).map_err(|e| io_err("cannot read", path, e))?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io_err("cannot read", path, e)),
    };
    let text = String::from_utf8(bytes).map_err(|_| unusable("it is not valid UTF-8 text"))?;
    let (token, name) = parse_token_file(&text).map_err(|reason| unusable(&reason))?;
    Ok(Some(SavedAccount { token, name, path: path.to_path_buf(), loose_mode: loose_mode_of(&meta) }))
}

// ---------------------------------------------------------------------------------------------
// Writing
// ---------------------------------------------------------------------------------------------

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Create a new file (fails if it exists), mode 0600 on unix.
fn create_private_new(path: &Path) -> io::Result<File> {
    let mut o = OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    o.open(path)
}

/// A fresh private temporary file next to `path`. Its name starts with `.` and does not end in
/// `.token`, so no lookup mistakes it for an account.
fn create_temp(path: &Path) -> io::Result<(PathBuf, File)> {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut attempt = 0;
    loop {
        let n = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0);
        let tmp = path.with_file_name(format!(".{name}.tmp-{}-{n}-{nanos:08x}", std::process::id()));
        match create_private_new(&tmp) {
            Ok(f) => return Ok((tmp, f)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists && attempt < 8 => attempt += 1,
            Err(e) => return Err(e),
        }
    }
}

/// Best-effort fsync of the directory so a new name in it survives a crash.
fn sync_dir(path: &Path) {
    if let Some(dir) = path.parent() {
        if let Ok(d) = File::open(dir) {
            let _ = d.sync_all();
        }
    }
}

/// How a content writer fills a file (a seam so tests can make the write fail half way).
type Writer<'a> = &'a dyn Fn(&mut File) -> io::Result<()>;

fn write_all_synced(file: &mut File, write: Writer<'_>) -> io::Result<()> {
    write(file)?;
    file.flush()?;
    file.sync_all()
}

enum Publish {
    /// The name is taken; nothing was written.
    Exists,
    Failed(io::Error),
}

/// Create `path` with the content `write` produces, only if it does not exist. The content is
/// complete and durable before the name appears (temporary file, fsync, then `link(2)`, which
/// fails if the name is taken), so a concurrent reader never sees a partial file and a failure
/// leaves nothing behind. On a file system without hard links it falls back to creating the file
/// in place, and removes it again if the write fails.
fn publish_new(path: &Path, write: Writer<'_>) -> Result<(), Publish> {
    let (tmp, mut file) = create_temp(path).map_err(Publish::Failed)?;
    let written = write_all_synced(&mut file, write);
    drop(file);
    if let Err(e) = written {
        let _ = fs::remove_file(&tmp);
        return Err(Publish::Failed(e));
    }
    let linked = fs::hard_link(&tmp, path);
    let _ = fs::remove_file(&tmp);
    match linked {
        Ok(()) => {
            sync_dir(path);
            Ok(())
        }
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Err(Publish::Exists),
        // No hard links here (or another reason the link failed): try the plain exclusive create,
        // which reports the real problem if there is one.
        Err(_) => publish_new_in_place(path, write),
    }
}

/// The fallback of [`publish_new`]: an exclusive create of `path` itself.
fn publish_new_in_place(path: &Path, write: Writer<'_>) -> Result<(), Publish> {
    let mut file = match create_private_new(path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => return Err(Publish::Exists),
        Err(e) => return Err(Publish::Failed(e)),
    };
    let written = write_all_synced(&mut file, write);
    drop(file);
    if let Err(e) = written {
        // We created this file a moment ago, so removing it is safe.
        let _ = fs::remove_file(path);
        return Err(Publish::Failed(e));
    }
    sync_dir(path);
    Ok(())
}

/// Replace `path` (or create it) with the content `write` produces: temporary file, fsync,
/// `rename(2)` over the old file. A failure leaves the old file exactly as it was.
fn publish_replacing(path: &Path, write: Writer<'_>) -> io::Result<()> {
    let (tmp, mut file) = create_temp(path)?;
    let written = write_all_synced(&mut file, write);
    drop(file);
    if let Err(e) = written {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    if let Err(e) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    sync_dir(path);
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// The store
// ---------------------------------------------------------------------------------------------

/// Atomically replace (or create) a small private file next to the token files with `text`:
/// temporary file, fsync, `rename(2)`. A failure leaves the old file as it was. The character
/// menu keeps its display-only notes about saved characters this way (see `ui::looks`).
pub fn write_private_atomic(path: &Path, text: &str) -> io::Result<()> {
    publish_replacing(path, &|f: &mut File| f.write_all(text.as_bytes()))
}

/// Every saved account of one server, for the character menu: the usable ones, and a line for
/// each file that could not be used (a menu lists what works instead of refusing to start).
#[derive(Debug, Clone, Default)]
pub struct AccountList {
    /// The default file's account first, then the named ones in file-name order. One entry per
    /// display name (compared case-insensitively), the default file winning, as in `find`.
    pub accounts: Vec<SavedAccount>,
    /// Why a candidate file was skipped; never contains a token.
    pub problems: Vec<String>,
}

/// The token files of one client state directory.
#[derive(Debug, Clone)]
pub struct TokenStore {
    dir: PathBuf,
}

impl TokenStore {
    /// The store under `<state_dir>/accounts/` (nothing is created until a token is saved).
    pub fn new(state_dir: &Path) -> Self {
        Self { dir: state_dir.join(ACCOUNTS_DIR) }
    }

    /// Every account saved for `server`. Unlike [`find`](Self::find) this never fails as a whole:
    /// a file that cannot be used is reported in [`AccountList::problems`] and the rest is
    /// listed.
    pub fn list(&self, server: &str) -> AccountList {
        let mut list = AccountList::default();
        let mut seen: Vec<String> = Vec::new();
        let mut take = |list: &mut AccountList, account: SavedAccount| {
            let key = account.name.to_lowercase();
            if !seen.contains(&key) {
                seen.push(key);
                list.accounts.push(account);
            }
        };
        match read_account(&self.default_path(server)) {
            Ok(Some(account)) => take(&mut list, account),
            Ok(None) => {}
            Err(e) => list.problems.push(e.to_string()),
        }
        let prefix = format!("{}.", server_key(server));
        let entries = match fs::read_dir(&self.dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return list,
            Err(e) => {
                list.problems.push(io_err("cannot list", &self.dir, e).to_string());
                return list;
            }
        };
        let mut candidates: Vec<(PathBuf, String)> = entries
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let file_name = entry.file_name();
                let file_name = file_name.to_str()?;
                let key = file_name.strip_prefix(&prefix)?.strip_suffix(TOKEN_SUFFIX)?;
                (!key.is_empty()).then(|| (entry.path(), key.to_string()))
            })
            .collect();
        candidates.sort();
        for (path, key) in candidates {
            match read_account(&path) {
                // A file only counts if the name stored in it belongs to its file name (another
                // server's default file can start with this server's key).
                Ok(Some(account)) if name_key(&account.name) == key => take(&mut list, account),
                Ok(_) => {}
                Err(e) => list.problems.push(e.to_string()),
            }
        }
        list
    }

    /// Forget a saved login: delete its token file from THIS machine. The server is not told and
    /// keeps the account, but without the token nobody on this machine can log in as it again, so
    /// callers ask first. Only a token file of this store, for `server`, that still holds exactly
    /// this account and token is removed: a file replaced since the account was listed (a new
    /// registration under the same file name) is refused, never deleted.
    pub fn forget(&self, server: &str, account: &SavedAccount) -> Result<(), AccountsError> {
        let in_store = account.path.parent() == Some(self.dir.as_path());
        let named_for_server = account
            .path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with(&server_key(server)) && n.ends_with(TOKEN_SUFFIX));
        if !in_store || !named_for_server {
            return Err(AccountsError::Forget("that is not a saved login of this server".into()));
        }
        match read_account(&account.path)? {
            // Already gone (forgotten in another window): nothing left to do.
            None => Ok(()),
            Some(current) if names_equal(&current.name, &account.name) && current.token == account.token => {
                fs::remove_file(&account.path).map_err(|e| io_err("cannot remove", &account.path, e))?;
                sync_dir(&account.path);
                Ok(())
            }
            Some(_) => Err(AccountsError::Forget(
                "the saved login changed since it was listed; it was not removed".into(),
            )),
        }
    }

    /// The default account file of `server`.
    fn default_path(&self, server: &str) -> PathBuf {
        self.dir.join(format!("{}{TOKEN_SUFFIX}", server_key(server)))
    }

    /// The name-specific account file of `name` on `server`.
    fn named_path(&self, server: &str, name: &str) -> PathBuf {
        self.dir.join(format!("{}.{}{TOKEN_SUFFIX}", server_key(server), name_key(name)))
    }

    /// Every name-specific account saved for `server`, in file-name order. A file only counts if
    /// the name stored in it belongs to its file name (another server's default file can start
    /// with this server's key). An unusable candidate is an error, not a guess.
    fn named_accounts(&self, server: &str) -> Result<Vec<SavedAccount>, AccountsError> {
        let prefix = format!("{}.", server_key(server));
        let entries = match fs::read_dir(&self.dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(io_err("cannot list", &self.dir, e)),
        };
        let mut found = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| io_err("cannot list", &self.dir, e))?;
            let file_name = entry.file_name();
            let Some(file_name) = file_name.to_str() else { continue };
            let Some(rest) = file_name.strip_prefix(&prefix) else { continue };
            let Some(key) = rest.strip_suffix(TOKEN_SUFFIX) else { continue };
            if key.is_empty() {
                continue;
            }
            if let Some(account) = read_account(&entry.path())? {
                if name_key(&account.name) == key {
                    found.push(account);
                }
            }
        }
        found.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(found)
    }

    /// The saved account to log in with on `server`.
    ///
    /// * `wanted == None`: the default file if there is one; else the only name-specific file; if
    ///   there are several, [`AccountsError::Ambiguous`] (the caller must pass a name).
    /// * `wanted == Some(n)`: the default file if the name stored in it is `n` (case-insensitively);
    ///   else the name-specific file for `n` if the name stored in it is `n`; else `None`. An
    ///   account of another name is never returned.
    ///
    /// `Ok(None)` means nothing is saved for that request. A file that cannot be used is an error
    /// (including a default file that cannot be read when a name is asked for: it may be the very
    /// account that was asked for).
    pub fn find(&self, server: &str, wanted: Option<&str>) -> Result<Option<SavedAccount>, AccountsError> {
        let default = read_account(&self.default_path(server))?;
        match wanted {
            None => {
                if default.is_some() {
                    return Ok(default);
                }
                let mut named = self.named_accounts(server)?;
                match named.len() {
                    0 => Ok(None),
                    1 => Ok(named.pop()),
                    _ => Err(AccountsError::Ambiguous {
                        names: named.iter().map(|a| clean(&a.name)).collect::<Vec<_>>().join(", "),
                    }),
                }
            }
            Some(name) => {
                if let Some(account) = default {
                    if names_equal(&account.name, name) {
                        return Ok(Some(account));
                    }
                }
                // A file under this name's key that stores another name is another account that
                // happens to share the key: not ours.
                Ok(read_account(&self.named_path(server, name))?.filter(|a| names_equal(&a.name, name)))
            }
        }
    }

    /// Does `path` hold the account `name`? (An unusable or missing file does not.)
    fn holds(path: &Path, name: &str) -> bool {
        matches!(read_account(path), Ok(Some(a)) if names_equal(&a.name, name))
    }

    /// Where a save of `name` would go right now, without writing anything: for a new account the
    /// default file if it is free, else the name-specific one; for `replacing`, the file that
    /// already holds this name. `Err` if nothing can be saved there. Advisory: another process may
    /// change things before the save, which then decides for itself (never overwriting).
    pub fn plan_save(&self, server: &str, name: &str, replacing: bool) -> Result<PathBuf, AccountsError> {
        let default = self.default_path(server);
        let named = self.named_path(server, name);
        if replacing {
            if Self::holds(&default, name) {
                return Ok(default);
            }
            if Self::holds(&named, name) {
                return Ok(named);
            }
        }
        let taken = |p: &Path| fs::symlink_metadata(p).is_ok();
        // A file that already holds THIS name is not "another account": a second file would be
        // shadowed by it (see `save_new`).
        if !taken(&default) {
            Ok(default)
        } else if Self::holds(&default, name) {
            Err(AccountsError::Exists { path: default })
        } else if !taken(&named) {
            Ok(named)
        } else {
            Err(AccountsError::Exists { path: named })
        }
    }

    fn ensure_dir(&self) -> Result<(), AccountsError> {
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&self.dir).map_err(|e| io_err("cannot create", &self.dir, e))
    }

    /// Make sure a token can be written here, before a registration makes an account that would
    /// need it: create the directory (mode 0700) and create and remove a private probe file.
    /// The one failure worse than not registering is registering and then not being able to keep
    /// the token.
    pub fn ensure_writable(&self) -> Result<(), AccountsError> {
        self.ensure_dir()?;
        let probe = self.dir.join("probe");
        let (tmp, file) = create_temp(&probe).map_err(|e| io_err("cannot write in", &self.dir, e))?;
        drop(file);
        fs::remove_file(&tmp).map_err(|e| io_err("cannot write in", &self.dir, e))
    }

    /// Save the token of a NEWLY registered account: an exclusive create of the default file; if
    /// that name is taken (another account, possibly written a moment ago by another process), an
    /// exclusive create of the name-specific file; if that is taken too, [`AccountsError::Exists`].
    /// Never overwrites, and never leaves two files for one name: if the file in the way already
    /// holds this very name, the old token would shadow the new one, so that is `Exists` as well
    /// (replacing it is what `--register` is for). Returns the file written.
    pub fn save_new(&self, server: &str, name: &str, token: &Secret) -> Result<PathBuf, AccountsError> {
        let text = format_token_file(token, name).map_err(AccountsError::Invalid)?;
        let write = |f: &mut File| f.write_all(text.as_bytes());
        self.ensure_dir()?;
        for path in [self.default_path(server), self.named_path(server, name)] {
            match publish_new(&path, &write) {
                Ok(()) => return Ok(path),
                Err(Publish::Exists) if Self::holds(&path, name) => return Err(AccountsError::Exists { path }),
                Err(Publish::Exists) => {}
                Err(Publish::Failed(e)) => return Err(io_err("cannot save the token to", &path, e)),
            }
        }
        Err(AccountsError::Exists { path: self.named_path(server, name) })
    }

    /// Save the token of an account registered again (`--register`): replace the file that holds
    /// this name (the default file if its stored name matches, else the name-specific file); if
    /// there is none, behave like [`save_new`](Self::save_new). A file of another account is never
    /// replaced. Returns the file written.
    pub fn save_replacing(&self, server: &str, name: &str, token: &Secret) -> Result<PathBuf, AccountsError> {
        let text = format_token_file(token, name).map_err(AccountsError::Invalid)?;
        let write = |f: &mut File| f.write_all(text.as_bytes());
        self.ensure_dir()?;
        for path in [self.default_path(server), self.named_path(server, name)] {
            if Self::holds(&path, name) {
                return publish_replacing(&path, &write)
                    .map(|()| path.clone())
                    .map_err(|e| io_err("cannot save the token to", &path, e));
            }
        }
        self.save_new(server, name, token)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, Barrier};

    const SERVER: &str = "127.0.0.1:47770";
    const KEY: &str = "127.0.0.1_47770";

    fn tok(s: &str) -> Secret {
        Secret::new(s)
    }

    fn store() -> (tempfile::TempDir, TokenStore) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = TokenStore::new(dir.path());
        (dir, store)
    }

    /// File names in the accounts directory, sorted (temporary files included).
    fn listing(store: &TokenStore) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(&store.dir)
            .map(|rd| rd.map(|e| e.expect("entry").file_name().to_string_lossy().into_owned()).collect())
            .unwrap_or_default();
        names.sort();
        names
    }

    #[cfg(unix)]
    fn mode(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(path).expect("metadata").permissions().mode() & 0o777
    }

    // ---- keys ------------------------------------------------------------------------------

    #[test]
    fn server_keys_are_lowercase_file_safe_and_capped() {
        assert_eq!(server_key("127.0.0.1:47770"), KEY);
        assert_eq!(server_key("Example.TEST:9000"), "example.test_9000");
        // IPv6 brackets and colons become underscores.
        assert_eq!(server_key("[::1]:47770"), "___1__47770");
        assert_eq!(server_key("[fe80::1]:80"), "_fe80__1__80");
        // Slashes, spaces and non-ASCII can never reach a path.
        assert_eq!(server_key("a/b c\u{e9}:1"), "a_b_c__1");
        assert!(!server_key("../../etc:1").contains('/'));
        assert_eq!(server_key(&"x".repeat(200)).len(), 80);
        assert_eq!(server_key(""), "_", "never empty");
    }

    #[test]
    fn name_keys_are_lowercase_file_safe_and_capped() {
        assert_eq!(name_key("Cloud"), "cloud");
        assert_eq!(name_key("Red XIII"), "red_xiii");
        assert_eq!(name_key("a.b-c"), "a.b-c");
        assert_eq!(name_key("\u{30af}\u{30e9}"), "__");
        assert_eq!(name_key("a/../b"), "a_.._b");
        assert_eq!(name_key(&"n".repeat(100)).len(), 40);
        assert_eq!(name_key(""), "_");
    }

    // ---- the file format -------------------------------------------------------------------

    #[test]
    fn the_file_format_is_two_lines_and_round_trips() {
        let text = format_token_file(&tok("fmvt_abc123"), "Cloud Strife").unwrap();
        assert_eq!(text, "fmvt_abc123\nname=Cloud Strife\n");
        let (token, name) = parse_token_file(&text).unwrap();
        assert_eq!((token.expose(), name.as_str()), ("fmvt_abc123", "Cloud Strife"));
        // Tolerated: no final newline, CRLF line ends.
        for ok in ["t0k\nname=N", "t0k\nname=N\n", "t0k\r\nname=N\r\n", "t0k\r\nname=N"] {
            let (token, name) = parse_token_file(ok).unwrap_or_else(|e| panic!("{ok:?}: {e}"));
            assert_eq!((token.expose(), name.as_str()), ("t0k", "N"), "{ok:?}");
        }
        // The token limits: 256 bytes is fine, 257 is not.
        assert!(parse_token_file(&format!("{}\nname=N\n", "a".repeat(256))).is_ok());
        assert!(parse_token_file(&format!("{}\nname=N\n", "a".repeat(257))).is_err());
    }

    #[test]
    fn unusable_files_are_rejected_with_a_reason_that_never_repeats_the_token() {
        let secret = "fmvt_SECRETSECRET";
        let bad = [
            ("".to_string(), "empty"),
            ("  \n\n".to_string(), "empty"),
            (format!("{secret}\n"), "name="),
            (secret.to_string(), "name="),
            (format!("{secret}\nnam=N\n"), "name="),
            (format!("{secret}\nname=\n"), "empty"),
            ("\nname=N\n".to_string(), "token is empty"),
            (format!("{secret} x\nname=N\n"), "visible ASCII"),
            (format!("{secret}\tx\nname=N\n"), "visible ASCII"),
            (format!("{secret}\u{e9}\nname=N\n"), "visible ASCII"),
            (format!("{}\nname=N\n", secret.repeat(40)), "longer than 256"),
            (format!("{secret}\nname=N\nextra\n"), "extra lines"),
            (format!("{secret}\nname=N\n\n"), "extra lines"),
            (format!("{secret}\nname=a\u{7}b\n"), "control character"),
            (format!("name=N\n{secret}\n"), "line 2 is not"),
            (format!("{}\nname=N\n", "z".repeat(2000)), "larger than 1 KiB"),
        ];
        for (text, needle) in bad {
            let err = parse_token_file(&text).expect_err(&text);
            assert!(err.contains(needle), "{text:?} -> {err}");
            assert!(!err.contains("SECRET"), "the reason must not repeat the token: {err}");
        }
        // Writing refuses what reading would refuse.
        assert!(format_token_file(&tok(""), "N").is_err());
        assert!(format_token_file(&tok("has space"), "N").is_err());
        assert!(format_token_file(&tok("t"), "").is_err());
        assert!(format_token_file(&tok("t"), "two\nlines").is_err());
        let err = format_token_file(&tok("tok en"), "N").unwrap_err();
        assert!(!err.contains("tok en"), "{err}");
    }

    // ---- reading files from disk -----------------------------------------------------------

    fn write_file(path: &Path, bytes: &[u8]) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }

    #[test]
    fn a_corrupt_oversized_or_garbage_file_is_reported_not_guessed() {
        let (_d, s) = store();
        let default = s.default_path(SERVER);
        let unusable = |s: &TokenStore| match s.find(SERVER, None) {
            Err(AccountsError::Unusable { path, reason }) => {
                assert_eq!(path, s.default_path(SERVER));
                reason
            }
            other => panic!("expected Unusable, got {other:?}"),
        };

        // A token without a name line.
        write_file(&default, b"fmvt_onlytoken\n");
        assert!(unusable(&s).contains("name="));
        // Binary garbage.
        write_file(&default, &[0xff, 0xfe, 0x00, 0x80, 0x81]);
        assert!(unusable(&s).contains("UTF-8"));
        // Oversized (a valid first two lines, then a lot of padding).
        let mut big = b"fmvt_tok\nname=N\n".to_vec();
        big.resize(5000, b'x');
        write_file(&default, &big);
        assert!(unusable(&s).contains("1 KiB"));
        // A directory where the file should be.
        fs::remove_file(&default).unwrap();
        fs::create_dir(&default).unwrap();
        assert!(unusable(&s).contains("regular file"));
        // The same file is reported when a name is asked for: it may be the account asked for.
        assert!(matches!(s.find(SERVER, Some("N")), Err(AccountsError::Unusable { .. })));
        // Nothing was modified or deleted by looking.
        assert!(default.is_dir());
        // The error text never carries file content.
        fs::remove_dir(&default).unwrap();
        write_file(&default, b"fmvt_SECRETSECRET\n");
        let text = s.find(SERVER, None).unwrap_err().to_string();
        assert!(text.contains(KEY) && !text.contains("SECRET"), "{text}");
    }

    // ---- find ------------------------------------------------------------------------------

    fn put(s: &TokenStore, file: &str, token: &str, name: &str) -> PathBuf {
        let path = s.dir.join(file);
        write_file(&path, format_token_file(&tok(token), name).unwrap().as_bytes());
        path
    }

    #[test]
    fn nothing_saved_is_not_an_error() {
        let (_d, s) = store();
        assert!(s.find(SERVER, None).unwrap().is_none());
        assert!(s.find(SERVER, Some("Cloud")).unwrap().is_none());
        // Looking never creates anything.
        assert!(!s.dir.exists());
    }

    #[test]
    fn find_without_a_name_prefers_the_default_file_then_the_only_named_one() {
        let (_d, s) = store();
        put(&s, &format!("{KEY}.tifa.token"), "tok_tifa", "Tifa");
        // Exactly one name-specific file and no default: that one.
        let a = s.find(SERVER, None).unwrap().expect("tifa");
        assert_eq!((a.token.expose(), a.name.as_str()), ("tok_tifa", "Tifa"));
        // A default file wins over name-specific ones.
        put(&s, &format!("{KEY}.token"), "tok_cloud", "Cloud");
        let a = s.find(SERVER, None).unwrap().expect("cloud");
        assert_eq!((a.token.expose(), a.name.as_str()), ("tok_cloud", "Cloud"));
        // Another server's files are never considered.
        assert!(s.find("10.0.0.9:1", None).unwrap().is_none());
    }

    #[test]
    fn several_named_files_without_a_default_are_ambiguous() {
        let (_d, s) = store();
        put(&s, &format!("{KEY}.tifa.token"), "tok_tifa", "Tifa");
        put(&s, &format!("{KEY}.aerith.token"), "tok_aerith", "Aerith");
        match s.find(SERVER, None) {
            Err(AccountsError::Ambiguous { names }) => {
                assert!(names.contains("Tifa") && names.contains("Aerith"), "{names}");
            }
            other => panic!("expected Ambiguous, got {other:?}"),
        }
        let text = s.find(SERVER, None).unwrap_err().to_string();
        assert!(text.contains("--name") && !text.contains("tok_"), "{text}");
        // Asking for a name resolves it.
        assert_eq!(s.find(SERVER, Some("aerith")).unwrap().unwrap().token.expose(), "tok_aerith");
        assert_eq!(s.find(SERVER, Some("TIFA")).unwrap().unwrap().token.expose(), "tok_tifa");
        assert!(s.find(SERVER, Some("Barret")).unwrap().is_none());
    }

    #[test]
    fn find_with_a_name_never_returns_another_account() {
        let (_d, s) = store();
        // The default file belongs to Cloud.
        put(&s, &format!("{KEY}.token"), "tok_cloud", "Cloud");
        assert_eq!(s.find(SERVER, Some("cloud")).unwrap().unwrap().token.expose(), "tok_cloud", "case-insensitive");
        assert_eq!(s.find(SERVER, Some("CLOUD")).unwrap().unwrap().name, "Cloud");
        // A default file of a DIFFERENT name is never used for Tifa.
        assert!(s.find(SERVER, Some("Tifa")).unwrap().is_none());
        // ...but Tifa's own file is.
        put(&s, &format!("{KEY}.tifa.token"), "tok_tifa", "Tifa");
        assert_eq!(s.find(SERVER, Some("Tifa")).unwrap().unwrap().token.expose(), "tok_tifa");
    }

    #[test]
    fn two_names_with_one_key_never_log_in_as_each_other() {
        let (_d, s) = store();
        assert_eq!(name_key("Red XIII"), name_key("red_xiii"));
        // `red_xiii` holds the file under that key; "Red XIII" is a different account.
        put(&s, &format!("{KEY}.red_xiii.token"), "tok_underscore", "red_xiii");
        assert!(s.find(SERVER, Some("Red XIII")).unwrap().is_none(), "the stored name decides, not the key");
        assert_eq!(s.find(SERVER, Some("red_xiii")).unwrap().unwrap().token.expose(), "tok_underscore");
    }

    #[test]
    fn a_file_whose_name_does_not_fit_its_file_name_is_ignored_when_listing() {
        let (_d, s) = store();
        // Another server's DEFAULT file can start with this server's key: it is not ours.
        put(&s, &format!("{KEY}.x_80.token"), "tok_other_server", "Barret");
        assert!(s.find(SERVER, None).unwrap().is_none(), "Barret is not stored under the key x_80");
        // Unrelated files and the leftovers of a failed write are ignored too.
        put(&s, "notes.txt", "t", "n");
        put(&s, &format!(".{KEY}.token.tmp-1-2-3"), "tok_tmp", "Tmp");
        assert!(s.find(SERVER, None).unwrap().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn a_token_file_other_users_can_read_is_flagged() {
        use std::os::unix::fs::PermissionsExt;
        let (_d, s) = store();
        let path = put(&s, &format!("{KEY}.token"), "tok", "Cloud");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(s.find(SERVER, None).unwrap().unwrap().loose_mode, None);
        for loose in [0o644, 0o640, 0o604, 0o666] {
            fs::set_permissions(&path, fs::Permissions::from_mode(loose)).unwrap();
            let found = s.find(SERVER, None).unwrap().unwrap();
            assert_eq!(found.loose_mode, Some(loose));
            let warning = loose_mode_warning(&found.path, loose);
            assert!(warning.contains(&format!("{loose:03o}")) && warning.contains("chmod 600"), "{warning}");
            assert!(!warning.contains("tok\n"), "{warning}");
        }
    }

    // ---- saving ----------------------------------------------------------------------------

    #[test]
    fn a_saved_token_is_private_complete_and_leaves_nothing_else_behind() {
        let (_d, s) = store();
        let path = s.save_new(SERVER, "Cloud", &tok("fmvt_secret1")).unwrap();
        assert_eq!(path, s.dir.join(format!("{KEY}.token")));
        assert_eq!(fs::read_to_string(&path).unwrap(), "fmvt_secret1\nname=Cloud\n");
        assert_eq!(listing(&s), vec![format!("{KEY}.token")], "no temporary file is left");
        #[cfg(unix)]
        {
            assert_eq!(mode(&path), 0o600);
            assert_eq!(mode(&s.dir), 0o700);
        }
        let found = s.find(SERVER, None).unwrap().unwrap();
        assert_eq!((found.token.expose(), found.name.as_str()), ("fmvt_secret1", "Cloud"));
        assert_eq!(found.loose_mode, None);
    }

    #[test]
    fn a_second_account_goes_to_its_own_file_and_a_third_collision_is_an_error() {
        let (_d, s) = store();
        let a = s.save_new(SERVER, "A", &tok("tok_a")).unwrap();
        let b = s.save_new(SERVER, "B", &tok("tok_b")).unwrap();
        assert_eq!(a, s.dir.join(format!("{KEY}.token")));
        assert_eq!(b, s.dir.join(format!("{KEY}.b.token")));
        // Same name again: both of its files are taken, nothing is overwritten.
        match s.save_new(SERVER, "B", &tok("tok_b2")) {
            Err(AccountsError::Exists { path }) => assert_eq!(path, b),
            other => panic!("expected Exists, got {other:?}"),
        }
        assert_eq!(s.find(SERVER, Some("B")).unwrap().unwrap().token.expose(), "tok_b");
        assert_eq!(s.find(SERVER, Some("A")).unwrap().unwrap().token.expose(), "tok_a");
        // The same name on another server has its own files.
        assert!(s.save_new("10.0.0.9:1", "B", &tok("tok_b3")).is_ok());
        // A name that is saved already is not saved a second time next to the first (the old
        // token would shadow the new one): the default file holds A, so A is refused too.
        match s.save_new(SERVER, "a", &tok("tok_a2")) {
            Err(AccountsError::Exists { path }) => assert_eq!(path, a),
            other => panic!("expected Exists, got {other:?}"),
        }
        assert!(matches!(s.plan_save(SERVER, "A", false), Err(AccountsError::Exists { .. })));
        assert_eq!(listing(&s).len(), 3, "A, B and the other server's file; no new one: {:?}", listing(&s));
        // `plan_save` says what a save would do, without doing it.
        let before = listing(&s);
        assert!(matches!(s.plan_save(SERVER, "B", false), Err(AccountsError::Exists { .. })));
        assert_eq!(s.plan_save(SERVER, "C", false).unwrap(), s.dir.join(format!("{KEY}.c.token")));
        assert_eq!(s.plan_save(SERVER, "B", true).unwrap(), b);
        assert_eq!(s.plan_save(SERVER, "A", true).unwrap(), a);
        assert_eq!(listing(&s), before);
        // Names that share a key: the second cannot be saved while the first holds the slot.
        let (_d2, s2) = store();
        s2.save_new(SERVER, "first", &tok("t1")).unwrap();
        s2.save_new(SERVER, "Red XIII", &tok("t2")).unwrap();
        assert!(matches!(s2.save_new(SERVER, "red_xiii", &tok("t3")), Err(AccountsError::Exists { .. })));
        assert!(matches!(s2.plan_save(SERVER, "red_xiii", false), Err(AccountsError::Exists { .. })));
        assert!(s2.find(SERVER, Some("red_xiii")).unwrap().is_none(), "Red XIII's file is not red_xiii's");
    }

    #[test]
    fn a_token_that_cannot_be_stored_is_refused_without_writing_or_echoing_it() {
        let (_d, s) = store();
        for bad in ["", "with space", "new\nline", &"t".repeat(300)] {
            let err = s.save_new(SERVER, "Cloud", &tok(bad)).unwrap_err();
            assert!(matches!(err, AccountsError::Invalid(_)), "{err}");
            if !bad.is_empty() {
                assert!(!err.to_string().contains(bad), "{err}");
            }
        }
        assert!(matches!(s.save_new(SERVER, "", &tok("t")), Err(AccountsError::Invalid(_))));
        assert!(!s.dir.exists(), "nothing was created");
    }

    /// Two bots that share one state directory and one server: whichever starts first, they end
    /// up with two separate files, and each logs in as itself.
    fn assert_two_separate_accounts(s: &TokenStore) {
        let files = listing(s);
        assert_eq!(files.len(), 2, "{files:?}");
        assert!(files.iter().all(|f| !f.starts_with('.')), "no temporary leftovers: {files:?}");
        let a = s.find(SERVER, Some("botA")).unwrap().expect("A's account");
        let b = s.find(SERVER, Some("botB")).unwrap().expect("B's account");
        assert_eq!((a.token.expose(), a.name.as_str()), ("tok_A", "botA"));
        assert_eq!((b.token.expose(), b.name.as_str()), ("tok_B", "botB"));
        assert_ne!(a.path, b.path);
        // Without a name, the default account is what you get (and it is a whole account).
        let default = s.find(SERVER, None).unwrap().expect("default");
        assert_eq!(default.path, s.dir.join(format!("{KEY}.token")));
        #[cfg(unix)]
        {
            assert_eq!(mode(&a.path), 0o600);
            assert_eq!(mode(&b.path), 0o600);
        }
    }

    fn register(s: &TokenStore, name: &'static str, token: &'static str) -> std::thread::JoinHandle<PathBuf> {
        let s = s.clone();
        std::thread::spawn(move || s.save_new(SERVER, name, &tok(token)).expect("save"))
    }

    #[test]
    fn two_bots_sharing_a_state_dir_a_then_b() {
        let (_d, s) = store();
        register(&s, "botA", "tok_A").join().unwrap();
        register(&s, "botB", "tok_B").join().unwrap();
        assert_two_separate_accounts(&s);
    }

    #[test]
    fn two_bots_sharing_a_state_dir_b_then_a() {
        let (_d, s) = store();
        register(&s, "botB", "tok_B").join().unwrap();
        register(&s, "botA", "tok_A").join().unwrap();
        // B took the default file this time; both are still found by name and are separate.
        let files = listing(&s);
        assert_eq!(files.len(), 2, "{files:?}");
        let a = s.find(SERVER, Some("botA")).unwrap().unwrap();
        let b = s.find(SERVER, Some("botB")).unwrap().unwrap();
        assert_eq!((a.token.expose(), b.token.expose()), ("tok_A", "tok_B"));
        assert_eq!(b.path, s.dir.join(format!("{KEY}.token")));
        assert_eq!(a.path, s.dir.join(format!("{KEY}.bota.token")));
    }

    /// Truly concurrent: both threads are released by a barrier and race for the default file.
    /// Repeated, because a race shows up only some of the time.
    #[test]
    fn two_bots_sharing_a_state_dir_truly_concurrent() {
        for round in 0..40 {
            let (_d, s) = store();
            let barrier = Arc::new(Barrier::new(2));
            let spawn = |name: &'static str, token: &'static str| {
                let (s, barrier) = (s.clone(), Arc::clone(&barrier));
                std::thread::spawn(move || {
                    barrier.wait();
                    s.save_new(SERVER, name, &tok(token)).expect("save")
                })
            };
            let (ta, tb) = (spawn("botA", "tok_A"), spawn("botB", "tok_B"));
            let (pa, pb) = (ta.join().unwrap(), tb.join().unwrap());
            assert_ne!(pa, pb, "round {round}: one file for two accounts");
            let files = listing(&s);
            assert_eq!(files.len(), 2, "round {round}: {files:?}");
            let a = s.find(SERVER, Some("botA")).unwrap().expect("A");
            let b = s.find(SERVER, Some("botB")).unwrap().expect("B");
            assert_eq!((a.token.expose(), b.token.expose()), ("tok_A", "tok_B"), "round {round}");
            assert_eq!((a.path, b.path), (pa, pb), "round {round}");
        }
    }

    /// A reader that looks while another process is saving sees the whole file or none at all.
    #[test]
    fn a_reader_never_sees_a_half_written_file() {
        for round in 0..100 {
            let (_d, s) = store();
            let barrier = Arc::new(Barrier::new(2));
            let done = Arc::new(AtomicBool::new(false));
            let reader = {
                let (s, barrier, done) = (s.clone(), Arc::clone(&barrier), Arc::clone(&done));
                std::thread::spawn(move || {
                    barrier.wait();
                    while !done.load(Ordering::Relaxed) {
                        match s.find(SERVER, Some("botA")) {
                            Ok(Some(a)) => assert_eq!(a.token.expose(), "tok_A"),
                            Ok(None) => {}
                            Err(e) => panic!("round {round}: a reader saw a partial or broken file: {e}"),
                        }
                    }
                })
            };
            barrier.wait();
            s.save_new(SERVER, "botA", &tok("tok_A")).unwrap();
            // Let the reader look at the finished file a few more times before it stops.
            std::thread::yield_now();
            done.store(true, Ordering::Relaxed);
            reader.join().unwrap();
            assert_eq!(s.find(SERVER, Some("botA")).unwrap().unwrap().token.expose(), "tok_A");
        }
    }

    #[test]
    fn replacing_overwrites_the_file_that_holds_the_name_and_nothing_else() {
        let (_d, s) = store();
        let a = s.save_new(SERVER, "Cloud", &tok("old_default")).unwrap();
        let b = s.save_new(SERVER, "Tifa", &tok("old_tifa")).unwrap();
        // The default file holds Cloud (the stored name matches case-insensitively).
        assert_eq!(s.save_replacing(SERVER, "CLOUD", &tok("new_cloud")).unwrap(), a);
        assert_eq!(s.find(SERVER, Some("Cloud")).unwrap().unwrap().token.expose(), "new_cloud");
        assert_eq!(s.find(SERVER, Some("Cloud")).unwrap().unwrap().name, "CLOUD", "the file now stores the new spelling");
        // Tifa lives in her own file; replacing her leaves Cloud alone.
        assert_eq!(s.save_replacing(SERVER, "Tifa", &tok("new_tifa")).unwrap(), b);
        assert_eq!(s.find(SERVER, Some("Tifa")).unwrap().unwrap().token.expose(), "new_tifa");
        assert_eq!(s.find(SERVER, Some("Cloud")).unwrap().unwrap().token.expose(), "new_cloud");
        // A name with no file behaves like a new registration (the default is taken, so: its own file).
        let c = s.save_replacing(SERVER, "Barret", &tok("tok_barret")).unwrap();
        assert_eq!(c, s.dir.join(format!("{KEY}.barret.token")));
        assert_eq!(listing(&s).len(), 3, "{:?}", listing(&s));
        #[cfg(unix)]
        for p in [&a, &b, &c] {
            assert_eq!(mode(p), 0o600, "{}", p.display());
        }
        // With nothing saved at all, it creates the default file.
        let (_d2, s2) = store();
        assert_eq!(s2.save_replacing(SERVER, "Cloud", &tok("t")).unwrap(), s2.dir.join(format!("{KEY}.token")));
    }

    #[test]
    fn replacing_never_touches_another_accounts_file() {
        let (_d, s) = store();
        // `red_xiii` holds the slot under the key both names share.
        s.save_new(SERVER, "first", &tok("t_first")).unwrap();
        s.save_new(SERVER, "red_xiii", &tok("t_underscore")).unwrap();
        // Registering "Red XIII" again must not overwrite `red_xiii`'s file.
        assert!(matches!(s.save_replacing(SERVER, "Red XIII", &tok("t_new")), Err(AccountsError::Exists { .. })));
        assert_eq!(s.find(SERVER, Some("red_xiii")).unwrap().unwrap().token.expose(), "t_underscore");
        assert_eq!(s.find(SERVER, Some("first")).unwrap().unwrap().token.expose(), "t_first");
    }

    // ---- failures leave no half-written file ------------------------------------------------

    /// A writer that writes half of its content and then fails (a full disk, say).
    fn failing_writer(f: &mut File) -> io::Result<()> {
        f.write_all(b"fmvt_half")?;
        Err(io::Error::other("disk full"))
    }

    #[test]
    fn a_failed_write_leaves_no_token_file_and_no_temporary_file() {
        let (_d, s) = store();
        s.ensure_dir().unwrap();
        let path = s.default_path(SERVER);
        match publish_new(&path, &failing_writer) {
            Err(Publish::Failed(e)) => assert!(e.to_string().contains("disk full")),
            _ => panic!("the write must fail"),
        }
        assert!(listing(&s).is_empty(), "{:?}", listing(&s));
        assert!(s.find(SERVER, None).unwrap().is_none());
        // The in-place fallback cleans up after itself as well.
        match publish_new_in_place(&path, &failing_writer) {
            Err(Publish::Failed(_)) => {}
            _ => panic!("the write must fail"),
        }
        assert!(listing(&s).is_empty(), "{:?}", listing(&s));
        // ...and respects an existing file.
        fs::write(&path, b"mine").unwrap();
        assert!(matches!(publish_new_in_place(&path, &failing_writer), Err(Publish::Exists)));
        assert_eq!(fs::read(&path).unwrap(), b"mine");
        assert!(matches!(publish_new(&path, &failing_writer), Err(Publish::Failed(_))));
        assert_eq!(fs::read(&path).unwrap(), b"mine", "an existing file is never touched");
    }

    #[test]
    fn a_failed_replacement_keeps_the_old_token() {
        let (_d, s) = store();
        let path = s.save_new(SERVER, "Cloud", &tok("old_token")).unwrap();
        assert!(publish_replacing(&path, &failing_writer).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "old_token\nname=Cloud\n");
        assert_eq!(listing(&s), vec![format!("{KEY}.token")], "no temporary file is left");
        // The in-place fallback works like the main path when it is used for a normal save.
        let other = s.dir.join("other.token");
        let text = b"fmvt_x\nname=N\n";
        publish_new_in_place(&other, &|f: &mut File| f.write_all(text)).map_err(|_| ()).unwrap();
        assert_eq!(fs::read(&other).unwrap(), text);
        #[cfg(unix)]
        assert_eq!(mode(&other), 0o600);
    }

    #[test]
    fn the_preflight_check_creates_the_private_directory_and_leaves_nothing_behind() {
        let (_d, s) = store();
        s.ensure_writable().unwrap();
        assert!(s.dir.is_dir());
        #[cfg(unix)]
        assert_eq!(mode(&s.dir), 0o700);
        assert!(listing(&s).is_empty(), "{:?}", listing(&s));
        // Doing it again is fine.
        s.ensure_writable().unwrap();
        // ...and it fails, before anything is registered, where a token could not be kept.
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(ACCOUNTS_DIR), b"in the way").unwrap();
        assert!(matches!(TokenStore::new(dir.path()).ensure_writable(), Err(AccountsError::Io { .. })));
    }

    #[test]
    fn a_save_into_an_unwritable_place_is_an_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        // The accounts "directory" is a regular file.
        fs::write(dir.path().join(ACCOUNTS_DIR), b"not a directory").unwrap();
        let s = TokenStore::new(dir.path());
        let err = s.save_new(SERVER, "Cloud", &tok("fmvt_secret")).unwrap_err();
        assert!(matches!(err, AccountsError::Io { .. }), "{err}");
        assert!(!err.to_string().contains("fmvt_secret"), "{err}");
        assert!(s.find(SERVER, None).is_err() || s.find(SERVER, None).unwrap().is_none());
    }

    // ---- listing and forgetting (the character menu) ---------------------------------------

    fn names(list: &AccountList) -> Vec<&str> {
        list.accounts.iter().map(|a| a.name.as_str()).collect()
    }

    #[test]
    fn the_list_has_the_default_account_first_then_the_named_ones_in_file_order() {
        let (_d, s) = store();
        assert!(s.list(SERVER).accounts.is_empty() && s.list(SERVER).problems.is_empty(), "nothing saved, nothing wrong");
        s.save_new(SERVER, "Cloud", &tok("tok_cloud")).unwrap(); // the default file
        s.save_new(SERVER, "Tifa", &tok("tok_tifa")).unwrap();
        s.save_new(SERVER, "Aerith", &tok("tok_aerith")).unwrap();
        s.save_new(SERVER, "Barret", &tok("tok_barret")).unwrap();
        let list = s.list(SERVER);
        assert_eq!(names(&list), ["Cloud", "Aerith", "Barret", "Tifa"]);
        assert!(list.problems.is_empty());
        // Tokens are really there (the menu logs in with them) but never in a Debug print.
        assert_eq!(list.accounts[1].token.expose(), "tok_aerith");
        assert!(!format!("{list:?}").contains("tok_"), "{list:?}");
        // Another server sees none of them.
        assert!(s.list("10.0.0.9:1").accounts.is_empty());
    }

    #[test]
    fn the_list_survives_a_broken_file_and_says_which() {
        let (_d, s) = store();
        s.save_new(SERVER, "Cloud", &tok("tok_cloud")).unwrap();
        let tifa = s.save_new(SERVER, "Tifa", &tok("tok_tifa")).unwrap();
        s.save_new(SERVER, "Aerith", &tok("tok_aerith")).unwrap();
        fs::write(&tifa, "tok_but_no_name_line\n").unwrap();
        let list = s.list(SERVER);
        assert_eq!(names(&list), ["Cloud", "Aerith"], "the usable ones are listed");
        assert_eq!(list.problems.len(), 1);
        assert!(list.problems[0].contains("unusable") && list.problems[0].contains("tifa"), "{:?}", list.problems);
        assert!(!list.problems[0].contains("tok_"), "never a token: {:?}", list.problems);
        // `find` would have refused as a whole.
        assert!(s.find(SERVER, Some("Tifa")).is_err());
        // A broken DEFAULT file is a problem too, and does not hide the named accounts.
        let (_d2, s2) = store();
        let default = s2.save_new(SERVER, "Cloud", &tok("tok_cloud")).unwrap();
        s2.save_new(SERVER, "Tifa", &tok("tok_tifa")).unwrap();
        fs::write(&default, "garbage").unwrap();
        let list = s2.list(SERVER);
        assert_eq!(names(&list), ["Tifa"]);
        assert_eq!(list.problems.len(), 1);
    }

    #[test]
    fn the_list_skips_files_of_other_names_other_servers_and_leftovers() {
        let (_d, s) = store();
        s.save_new(SERVER, "Cloud", &tok("tok_cloud")).unwrap();
        // Another server's default file starting with this server's key, a temp file, a note.
        put(&s, &format!("{KEY}.x_80.token"), "tok_other_server", "Barret");
        put(&s, &format!(".{KEY}.token.tmp-1-2-3"), "tok_tmp", "Tmp");
        put(&s, "notes.txt", "t", "n");
        // The same name saved twice (a default file and a named one) is listed once.
        put(&s, &format!("{KEY}.cloud.token"), "tok_cloud_again", "cloud");
        let list = s.list(SERVER);
        assert_eq!(names(&list), ["Cloud"]);
        assert_eq!(list.accounts[0].token.expose(), "tok_cloud", "the default file wins, as in find()");
        assert!(list.problems.is_empty(), "{:?}", list.problems);
    }

    #[test]
    fn forgetting_deletes_that_one_token_file_and_nothing_else() {
        let (_d, s) = store();
        s.save_new(SERVER, "Cloud", &tok("tok_cloud")).unwrap();
        let tifa_path = s.save_new(SERVER, "Tifa", &tok("tok_tifa")).unwrap();
        s.save_new(SERVER, "Aerith", &tok("tok_aerith")).unwrap();
        let list = s.list(SERVER);
        let tifa = list.accounts.iter().find(|a| a.name == "Tifa").unwrap().clone();
        s.forget(SERVER, &tifa).unwrap();
        assert!(!tifa_path.exists());
        assert_eq!(names(&s.list(SERVER)), ["Cloud", "Aerith"]);
        // Forgetting what is already gone is not an error (another window did it).
        s.forget(SERVER, &tifa).unwrap();
        // The default account can be forgotten too, and the rest remains.
        let cloud = s.list(SERVER).accounts[0].clone();
        assert_eq!(cloud.name, "Cloud");
        s.forget(SERVER, &cloud).unwrap();
        assert_eq!(names(&s.list(SERVER)), ["Aerith"]);
        assert!(listing(&s).iter().all(|f| !f.contains("tmp")), "{:?}", listing(&s));
    }

    #[test]
    fn forgetting_never_removes_a_file_that_changed_or_is_not_a_login_of_this_store() {
        let (_d, s) = store();
        let path = s.save_new(SERVER, "Cloud", &tok("tok_old")).unwrap();
        let listed = s.list(SERVER).accounts[0].clone();
        // The file was replaced by a new registration under the same name since it was listed.
        s.save_replacing(SERVER, "Cloud", &tok("tok_new")).unwrap();
        let err = s.forget(SERVER, &listed).unwrap_err();
        assert!(matches!(err, AccountsError::Forget(_)), "{err}");
        assert!(path.exists(), "the new login is still there");
        assert_eq!(s.find(SERVER, None).unwrap().unwrap().token.expose(), "tok_new");
        // A path outside the store, or one that is not a token file of this server, is refused.
        let outside = tempfile::tempdir().unwrap();
        let stray = outside.path().join(format!("{KEY}.token"));
        fs::write(&stray, "tok\nname=Cloud\n").unwrap();
        let fake = SavedAccount { token: tok("tok"), name: "Cloud".into(), path: stray.clone(), loose_mode: None };
        assert!(s.forget(SERVER, &fake).is_err());
        assert!(stray.exists());
        let other_server = SavedAccount { path: s.dir.join("10.0.0.9_1.token"), ..fake.clone() };
        assert!(s.forget(SERVER, &other_server).is_err());
        let not_a_token = SavedAccount { path: s.dir.join("notes.txt"), ..fake };
        assert!(s.forget(SERVER, &not_a_token).is_err());
    }

    #[test]
    fn private_notes_are_replaced_atomically_and_leave_nothing_behind() {
        let (_d, s) = store();
        s.ensure_dir().unwrap();
        let path = s.dir.join("127.0.0.1_47770.cloud.look");
        write_private_atomic(&path, "first\n").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "first\n");
        write_private_atomic(&path, "second\n").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "second\n");
        #[cfg(unix)]
        assert_eq!(mode(&path), 0o600);
        assert_eq!(listing(&s), vec!["127.0.0.1_47770.cloud.look".to_string()], "no temporary file is left");
        // Notes are not logins: they never show up in the list or in find().
        assert!(s.list(SERVER).accounts.is_empty() && s.find(SERVER, None).unwrap().is_none());
    }
}
