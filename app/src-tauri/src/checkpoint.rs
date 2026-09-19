#![allow(dead_code)] // T1 only provides the store; engine hook callers arrive in later tasks.

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

mod archive;
mod content;
mod paths;
mod preview;
mod restore;
mod store;
mod types;
mod xattrs;

use archive::*;
use content::*;
use paths::*;
use preview::*;
use restore::*;
#[cfg(test)]
use store::UNRESOLVABLE_CURRENT_DIGEST;
pub(crate) use store::{changed_file_paths_for_session, CheckpointStore};
pub(crate) use types::*;
use xattrs::*;

#[cfg(test)]
mod tests;
