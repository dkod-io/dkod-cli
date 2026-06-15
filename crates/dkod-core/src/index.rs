//! Storage v2 rollup index (`dkod-index/1`, design 2026-06-10): one
//! `refs/dkod/index` ref pointing at a commit chain whose tree holds every
//! session's metadata + body and the commit/patch-id lookup tables. This
//! module owns the tree layout, the tree codec, and the batched, CAS-guarded
//! write path. `store.rs` composes it with the legacy-ref dual-write and the
//! permanent legacy read fallback.

/// The single v2 ref (§4.1). Points at a commit; parent = previous tip.
pub const INDEX_REF: &str = "refs/dkod/index";
/// Contents of the root `version` blob.
pub const INDEX_VERSION: &str = "1\n";

/// Unix milliseconds embedded in a UUIDv7's first 48 bits, or `None` for
/// anything that is not a v7 UUID (foreign imports, hand-rolled test ids).
pub(crate) fn uuid_v7_unix_ms(id: &str) -> Option<u64> {
    let u = uuid::Uuid::parse_str(id).ok()?;
    if u.get_version_num() != 7 {
        return None;
    }
    let b = u.as_bytes();
    Some(
        ((b[0] as u64) << 40)
            | ((b[1] as u64) << 32)
            | ((b[2] as u64) << 24)
            | ((b[3] as u64) << 16)
            | ((b[4] as u64) << 8)
            | (b[5] as u64),
    )
}

/// `YYYY-MM-DD` (UTC) for a unix-milliseconds timestamp. Civil-from-days
/// algorithm (Howard Hinnant) — dependency-free, valid for all dates ≥ 1970.
pub(crate) fn date_from_unix_ms(ms: u64) -> String {
    let days = (ms / 86_400_000) as i64;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{y:04}-{m:02}-{d:02}")
}

/// `sessions/<date>/<id>` for a v7 id (date computable from the id alone,
/// §4.2), `None` otherwise.
pub fn session_dir(id: &str) -> Option<String> {
    uuid_v7_unix_ms(id).map(|ms| format!("sessions/{}/{id}", date_from_unix_ms(ms)))
}

/// Write-side directory: v7 timestamp when available, else the session's
/// `created_at` (clamped to epoch) — §10's rule for non-v7 ids.
pub fn session_dir_for(id: &str, created_at: i64) -> String {
    match session_dir(id) {
        Some(d) => d,
        None => {
            let ms = (created_at.max(0) as u64).saturating_mul(1000);
            format!("sessions/{}/{id}", date_from_unix_ms(ms))
        }
    }
}

pub fn meta_path(id: &str) -> Option<String> {
    session_dir(id).map(|d| format!("{d}/meta.json"))
}

pub fn body_path(id: &str) -> Option<String> {
    session_dir(id).map(|d| format!("{d}/body.json"))
}

/// `commits/<aa>/<remaining-38>` (§4.2 hex fan-out). `sha` is always a full
/// git object hash (40 hex) by contract; panics on inputs shorter than 2.
pub fn commit_pointer_path(sha: &str) -> String {
    assert!(sha.len() >= 2, "commit sha must be at least 2 chars");
    format!("commits/{}/{}", &sha[..2], &sha[2..])
}

/// `patchid/<aa>/<remaining-38>`. `pid` is always a full patch-id hash by
/// contract; panics on inputs shorter than 2.
pub fn patchid_pointer_path(pid: &str) -> String {
    assert!(pid.len() >= 2, "patch-id must be at least 2 chars");
    format!("patchid/{}/{}", &pid[..2], &pid[2..])
}

#[cfg(test)]
mod path_tests {
    use super::*;

    /// UUIDv7 generated at exactly 2025-01-01T00:00:00Z (unix 1735689600).
    fn v7_at(secs: u64) -> String {
        uuid::Uuid::new_v7(uuid::Timestamp::from_unix(uuid::NoContext, secs, 0)).to_string()
    }

    #[test]
    fn v7_id_maps_to_its_embedded_date() {
        let id = v7_at(1735689600);
        assert_eq!(uuid_v7_unix_ms(&id), Some(1735689600000));
        assert_eq!(session_dir(&id), Some(format!("sessions/2025-01-01/{id}")));
        assert_eq!(
            meta_path(&id),
            Some(format!("sessions/2025-01-01/{id}/meta.json"))
        );
        assert_eq!(
            body_path(&id),
            Some(format!("sessions/2025-01-01/{id}/body.json"))
        );
    }

    #[test]
    fn non_v7_id_yields_none_paths() {
        assert_eq!(uuid_v7_unix_ms("not-a-uuid"), None);
        // v4 uuid: version nibble is 4, not 7.
        assert_eq!(
            uuid_v7_unix_ms("123e4567-e89b-42d3-a456-426614174000"),
            None
        );
        assert_eq!(session_dir("not-a-uuid"), None);
    }

    #[test]
    fn session_dir_for_falls_back_to_created_at() {
        // non-v7 id + created_at 2025-01-01 → date from created_at
        let d = session_dir_for("legacy-id-1", 1735689600);
        assert_eq!(d, "sessions/2025-01-01/legacy-id-1");
        // v7 id wins over created_at
        let id = v7_at(1735689600);
        assert_eq!(session_dir_for(&id, 0), format!("sessions/2025-01-01/{id}"));
        // negative created_at clamps to epoch
        assert_eq!(session_dir_for("x", -5), "sessions/1970-01-01/x");
    }

    #[test]
    fn date_math_handles_leap_years_and_epoch() {
        assert_eq!(date_from_unix_ms(0), "1970-01-01");
        // 2024-02-29T12:00:00Z = 1709208000
        assert_eq!(date_from_unix_ms(1709208000 * 1000), "2024-02-29");
        // 2026-06-10T00:00:00Z = 1781049600
        assert_eq!(date_from_unix_ms(1781049600 * 1000), "2026-06-10");
    }

    #[test]
    fn pointer_paths_use_two_hex_fanout() {
        let sha = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
        assert_eq!(
            commit_pointer_path(sha),
            format!("commits/de/{}", &sha[2..])
        );
        assert_eq!(
            patchid_pointer_path(sha),
            format!("patchid/de/{}", &sha[2..])
        );
    }
}
