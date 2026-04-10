use std::path::Path;

use axsys_noun::serdes::{Cue, Jam};
use axsys_noun::{Atom, Cell, Noun};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

use crate::errors::LauncherError;

const CONN_TIMEOUT_SECS: u64 = 10;

/// Connect to a running ship's conn.sock and retrieve the +code (web login code).
pub async fn get_code(pier_path: &Path) -> Result<String, LauncherError> {
    let sock_path = pier_path.join(".urb").join("conn.sock");

    let result = tokio::time::timeout(
        tokio::time::Duration::from_secs(CONN_TIMEOUT_SECS),
        do_get_code(&sock_path),
    )
    .await
    .map_err(|_| LauncherError::Runtime {
        reason: "conn.sock: timed out waiting for +code".into(),
    })?;

    result
}

async fn do_get_code(sock_path: &Path) -> Result<String, LauncherError> {
    let mut stream = UnixStream::connect(sock_path)
        .await
        .map_err(|e| LauncherError::Runtime {
            reason: format!("conn.sock: {e}"),
        })?;

    // [request-id %fyrd %base %code %noun %noun 0]
    // Runs /ted/code.hoon thread from %base desk.
    // The thread scries jael for the web login code and returns a @p.
    let request = Cell::from([
        Noun::from(Atom::from(0u64)),
        Noun::from(Atom::from("fyrd")),
        Noun::from(Atom::from("base")),
        Noun::from(Atom::from("code")),
        Noun::from(Atom::from("noun")),
        Noun::from(Atom::from("noun")),
        Noun::from(Atom::null()),
    ])
    .into_noun();

    // Jam → newt frame → send
    let jammed = request.jam();
    let payload = jammed.as_bytes();

    let mut frame = Vec::with_capacity(5 + payload.len());
    frame.push(0x00); // newt version tag
    frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    frame.extend_from_slice(payload);

    stream
        .write_all(&frame)
        .await
        .map_err(|e| LauncherError::Runtime {
            reason: format!("conn.sock write: {e}"),
        })?;

    // Read response newt frame
    let mut hdr = [0u8; 5];
    stream
        .read_exact(&mut hdr)
        .await
        .map_err(|e| LauncherError::Runtime {
            reason: format!("conn.sock read header: {e}"),
        })?;

    if hdr[0] != 0x00 {
        return Err(LauncherError::Runtime {
            reason: format!("conn.sock: unexpected newt version byte: {:#x}", hdr[0]),
        });
    }

    let len = u32::from_le_bytes([hdr[1], hdr[2], hdr[3], hdr[4]]) as usize;
    let mut body = vec![0u8; len];
    stream
        .read_exact(&mut body)
        .await
        .map_err(|e| LauncherError::Runtime {
            reason: format!("conn.sock read body: {e}"),
        })?;

    // Cue response noun
    let response = Noun::cue(Atom::from(body)).map_err(|e| LauncherError::Runtime {
        reason: format!("conn.sock cue: {e:?}"),
    })?;

    parse_avow_response(response)
}

/// Parse a %fyrd response: [request-id %avow result]
/// Success result: [0 %noun code-value]  (%.y + cage)
/// Failure result: [1 goof]              (%.n + error)
fn parse_avow_response(noun: Noun) -> Result<String, LauncherError> {
    let err = |msg: &str| LauncherError::Runtime {
        reason: format!("conn.sock response: {msg}"),
    };

    // [rid [tag result]]
    let cell = noun.as_cell().ok_or_else(|| err("not a cell"))?;
    let rest = cell.tail_ref().as_cell().ok_or_else(|| err("malformed"))?;

    let tag = rest
        .head_ref()
        .as_atom()
        .and_then(|a| a.as_str().ok())
        .unwrap_or("");
    if tag != "avow" {
        return Err(err(&format!("expected %avow, got %{tag}")));
    }

    // result = [flag ...]
    let result = rest
        .tail_ref()
        .as_cell()
        .ok_or_else(|| err("avow: missing result"))?;

    let flag = result
        .head_ref()
        .as_atom()
        .and_then(|a| a.as_u64())
        .unwrap_or(1);

    if flag != 0 {
        return Err(err("thread failed (%.n)"));
    }

    // cage = [%noun code-value]
    let cage = result
        .tail_ref()
        .as_cell()
        .ok_or_else(|| err("avow: missing cage"))?;

    let code_atom = cage
        .tail_ref()
        .as_atom()
        .ok_or_else(|| err("code is not an atom"))?;

    let code_int = code_atom.as_u64().ok_or_else(|| {
        err("code value exceeds u64 — unsupported ship class")
    })?;

    let code_str = urbit_ob::patp(code_int);
    Ok(code_str.strip_prefix('~').unwrap_or(&code_str).to_string())
}
