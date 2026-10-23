//! The fleet's token economy: capacity forecast, cadence mode, launch and resume gates.
//!
//! A port of the pure part of the workspace's `scripts/fleet_economy.py` (BRO-3013, BRO-3118),
//! held to it by `scripts/parity-delegation.py` and `crates/core/tests/parity_delegation.rs`.
//! Capacity is in percent of each account's 7-day window, the unit the usage endpoint gives
//! (`provider_manager.py --json usage`). The fleet's runway is its total 7d headroom over the sum
//! of the usable accounts' burn rates; relief is the earliest 7-day reset. A projection that runs
//! out before relief is what the cadence ladder slows for.
//!
//! Not ported, because they read files the coordinator tick owns until `loops-port`: the sample
//! cache (`update_samples`), transcript reading (`session_tokens`) and the economy ledger
//! (`record`, `report`). Times are microseconds since the epoch, Python's datetime precision.

use crate::delegation::Fault;
use crate::py;
use serde_json::{Map, Value, json};

/// Burn rate is measured over the last 24 h of samples.
pub const WINDOW_H: i64 = 24;
/// Fewer hours of samples than this: no rate.
pub const MIN_SPAN_H: f64 = 1.0;
/// Context (thousands of tokens) above which a session is a handoff candidate.
pub const HANDOFF_K: i64 = 400;
/// Driven sessions above this get a fresh successor, not a resume.
pub const SUCCESSOR_K: i64 = 600;
/// Runway shorter than this, and short of relief: critical.
pub const CRITICAL_RUNWAY_H: f64 = 12.0;
/// Every usable account at or over this 7d %: critical.
pub const CRITICAL_7D: f64 = 90.0;
/// The active account's 5h at or over this: read-only.
pub const READ_ONLY_5H: f64 = 95.0;
/// Every account that decides the gate under this 5h %: resumes may run.
pub const RESUME_5H: f64 = 90.0;
/// 7d at or over this, in a window not yet reset: the account is limited (BRO-3223).
pub const LIMIT_7D: f64 = 95.0;
/// The active account's last good numbers still count while this young (minutes). Mirrors
/// provider-manager's `staleNumbersMinutes`; `gate_bounds` passes the live value.
pub const STALE_OK_MIN: f64 = 30.0;
/// A standby at or under this 5h % is one the balancer would fail over to (`standbyMax`).
pub const STANDBY_MAX_5H: f64 = 70.0;
/// provider-manager `LIMITED_HOLD_SECONDS`: a probe-limited standby is no target this long.
pub const PROBE_HOLD_S: f64 = 3600.0;
/// provider-manager `SIGNAL_TTL_SECONDS`: a StopFailure report stays pending this long.
pub const SIGNAL_TTL_S: f64 = 900.0;

const US: i64 = 1_000_000;
const HOUR_US: i64 = 3600 * US;
/// `datetime.min` and `datetime.max` in UTC, as epoch microseconds: the dates Python can hold.
const DT_MIN_US: i64 = -62_135_596_800 * US;
const DT_MAX_US: i64 = 253_402_300_799 * US + 999_999;

/// A cadence mode, most permissive first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Mode {
    Normal,
    Conserve,
    Critical,
    ReadOnly,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Normal => "normal",
            Mode::Conserve => "conserve",
            Mode::Critical => "critical",
            Mode::ReadOnly => "read_only",
        }
    }

    /// The tick period in hours and the cap on concurrently driven sessions (none = no cap).
    pub fn rung(self) -> (i64, Option<i64>) {
        match self {
            Mode::Normal => (2, None),
            Mode::Conserve => (4, Some(3)),
            Mode::Critical => (6, Some(1)),
            Mode::ReadOnly => (6, Some(0)),
        }
    }
}

/// `parse_ts`: none for null or `""`; a number is epoch seconds, or milliseconds above 1e11; a
/// string is `YYYY-MM-DDTHH:MM:SS[.ffffff]` in UTC (`Z` or `+00:00`), the form the usage endpoint and
/// the samples write. Anything else, or an instant outside `datetime`'s years, is outside the
/// schema (`crate::schema`), which callers check first; here it is refused as `timestamp`.
pub fn parse_ts(v: Option<&Value>) -> Result<Option<i64>, Fault> {
    let refused = |why: &str| Fault::Refused { field: "timestamp".into(), why: why.into() };
    match v {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if s.is_empty() => Ok(None),
        Some(Value::String(s)) => timestamp_us(s).map(Some).ok_or_else(|| refused(TIME_FORM)),
        Some(Value::Number(n)) => epoch_us(n.as_f64().unwrap_or(f64::NAN)).map(Some).ok_or_else(|| refused(TIME_RANGE)),
        Some(_) => Err(refused(TIME_FORM)),
    }
}

/// What a timestamp string must be.
pub const TIME_FORM: &str = "a UTC timestamp YYYY-MM-DDTHH:MM:SS[.ffffff](Z|+00:00) within years 1..9999";
/// What an epoch timestamp must be.
pub const TIME_RANGE: &str = "epoch seconds (milliseconds above 1e11) within years 1..9999";

/// `datetime.fromtimestamp(f / (1000 if f > 1e11 else 1), utc)` as epoch µs: whole seconds, then
/// the fraction rounded half-even to µs, as CPython does. None outside `datetime`'s years.
pub fn epoch_us(f: f64) -> Option<i64> {
    let f = if f > 1e11 { f / 1000.0 } else { f };
    // Past ±9e12 s (285,000 years) no year Python holds is near, and the µs would overflow i64.
    if !f.is_finite() || f.abs() >= 9e12 {
        return None;
    }
    let whole = f.trunc();
    let us = whole as i64 * US + round_half_even((f - whole) * 1e6);
    (DT_MIN_US..=DT_MAX_US).contains(&us).then_some(us)
}

/// The strict UTC form as epoch µs, or None. `datetime.fromisoformat` reads every string this
/// accepts to the same instant.
pub fn timestamp_us(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    let num = |i: usize, n: usize| -> Option<i64> {
        let d = b.get(i..i + n)?;
        d.iter().all(u8::is_ascii_digit).then(|| d.iter().fold(0, |a, c| a * 10 + i64::from(c - b'0')))
    };
    let at = |i: usize, c: u8| b.get(i) == Some(&c);
    if !(at(4, b'-') && at(7, b'-') && at(10, b'T') && at(13, b':') && at(16, b':')) {
        return None;
    }
    let (y, m, d, hh, mm, ss) = (num(0, 4)?, num(5, 2)?, num(8, 2)?, num(11, 2)?, num(14, 2)?, num(17, 2)?);
    let mut i = 19;
    let mut us = 0;
    if at(i, b'.') {
        let n = b[i + 1..].iter().take_while(|c| c.is_ascii_digit()).count();
        if !(1..=6).contains(&n) {
            return None;
        }
        us = num(i + 1, n)? * 10i64.pow(6 - n as u32);
        i += 1 + n;
    }
    if !matches!(&s[i..], "Z" | "+00:00") {
        return None;
    }
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let dim = [31, if leap { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    if !(1..=9999).contains(&y) || !(1..=12).contains(&m) || !(1..=dim[m as usize - 1]).contains(&d) {
        return None;
    }
    if hh > 23 || mm > 59 || ss > 59 {
        return None;
    }
    Some(((days_from_civil(y, m, d) * 24 + hh) * 60 + mm) * 60 * US + ss * US + us)
}

/// Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's `days_from_civil`).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The date of a day since 1970-01-01 (Howard Hinnant's `civil_from_days`).
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

fn overflow() -> Fault {
    Fault::Crash { kind: "OverflowError", msg: "date value out of range".into() }
}

fn round_half_even(f: f64) -> i64 {
    let r = f.round();
    let r = if (f - f.trunc()).abs() == 0.5 { 2.0 * (f / 2.0).round() } else { r };
    r as i64
}

/// `datetime + timedelta(hours=h)` as CPython's `timedelta_new` builds it: the whole hours exactly,
/// the fraction times 3.6e9 µs in double precision, rounded half to even.
/// A sum outside `datetime`'s years raises, as Python's does: never a date that is not there.
pub fn add_hours(at: i64, hours: f64) -> Result<i64, Fault> {
    if hours.is_nan() {
        return Err(Fault::Value("cannot convert float NaN to integer".into()));
    }
    // 1e8 hours is over 11,000 years: past it every sum is out of range (and i64 µs overflow).
    if !hours.is_finite() || hours.abs() > 1e8 {
        return Err(overflow());
    }
    let whole = hours.trunc();
    let sum = at + whole as i64 * HOUR_US + round_half_even((hours - whole) * HOUR_US as f64);
    if (DT_MIN_US..=DT_MAX_US).contains(&sum) { Ok(sum) } else { Err(overflow()) }
}

/// `iso(d)`: UTC, whole seconds, `+00:00`. Every instant here is inside `datetime`'s years.
pub fn iso(us: Option<i64>) -> Value {
    let Some(us) = us else { return Value::Null };
    let secs = us.div_euclid(US);
    let (y, m, d) = civil_from_days(secs.div_euclid(86_400));
    let s = secs.rem_euclid(86_400);
    Value::String(format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}+00:00", s / 3600, s / 60 % 60, s % 60))
}

fn hours_between(later: i64, earlier: i64) -> f64 {
    (later - earlier) as f64 / 1e6 / 3600.0
}

/// One account's line in a sample, in the order the usage rows gave them.
pub type Accounts = Vec<(String, Map<String, Value>)>;

/// `sample_from_usage`: one cache line from the usage rows, per account (email local part).
/// A repeated name keeps its first position and its last row, as a Python dict does.
pub fn sample_from_usage(rows: &[Value], now: i64) -> Result<(Value, Accounts), Fault> {
    crate::schema::usage(rows)?;
    let mut accounts: Accounts = Vec::new();
    for a in rows {
        let name = py::str_of(a.get("email").unwrap_or(&json!("?")));
        let name = name.split('@').next().unwrap_or("").to_string();
        let raw = |k: &str| a.get(k).cloned().unwrap_or(Value::Null);
        let flag = |k: &str| Value::Bool(py::truthy(a.get(k)));
        let mut v = Map::new();
        v.insert("5h".into(), raw("fiveHourUtil"));
        v.insert("7d".into(), raw("sevenDayUtil"));
        v.insert("reset_7d".into(), raw("sevenDayResetsAt"));
        v.insert("reset_5h".into(), raw("fiveHourResetsAt"));
        v.insert("active".into(), flag("isActive"));
        v.insert("limited".into(), flag("isRateLimited"));
        v.insert("stale".into(), flag("stale"));
        v.insert("age_s".into(), raw("numbersAgeSeconds"));
        // No live credential: only the owner's login brings it back.
        let needs_login = py::truthy(a.get("needsLogin")) || a.get("hasStoredCredentials") == Some(&Value::Bool(false));
        v.insert("needs_login".into(), Value::Bool(needs_login));
        match accounts.iter_mut().find(|(n, _)| *n == name) {
            Some(slot) => slot.1 = v,
            None => accounts.push((name, v)),
        }
    }
    let map: Map<String, Value> = accounts.iter().map(|(n, v)| (n.clone(), Value::Object(v.clone()))).collect();
    Ok((json!({"at": iso(Some(now)), "accounts": map}), accounts))
}

/// Two 7d reset stamps name the same window (the endpoint jitters by sub-seconds).
fn same_window(a: Option<&Value>, b: Option<&Value>) -> Result<bool, Fault> {
    Ok(match (parse_ts(a)?, parse_ts(b)?) {
        (Some(x), Some(y)) => ((x - y) as f64 / 1e6).abs() < 3600.0,
        (x, y) => x.is_none() && y.is_none(),
    })
}

/// 7d percentage points per hour for one account over the last `WINDOW_H`, or none. Only the
/// current window counts: a different reset stamp, or a utilization drop, starts a new segment.
/// The samples are inside the schema (`forecast` checks them).
fn burn_rate(samples: &[Value], account: &str, now: i64) -> Result<Option<f64>, Fault> {
    let mut pts: Vec<(i64, f64, Option<Value>)> = Vec::new();
    for s in samples {
        let at = parse_ts(s.get("at"))?;
        let v = s.get("accounts").and_then(|a| a.get(account)).filter(|v| v.is_object());
        let field = |k: &str| v.and_then(|v| v.get(k));
        let (Some(at), Some(seven)) = (at, py::num(field("7d"))) else { continue };
        if py::truthy(field("stale")) {
            continue;
        }
        if (now - at) as f64 / 1e6 > (WINDOW_H * 3600) as f64 || at > now {
            continue;
        }
        pts.push((at, seven, field("reset_7d").cloned()));
    }
    pts.sort_by_key(|p| p.0);
    let mut seg: Vec<&(i64, f64, Option<Value>)> = Vec::new();
    for p in &pts {
        if let Some(last) = seg.last()
            && (p.1 < last.1 || !same_window(p.2.as_ref(), last.2.as_ref())?)
        {
            seg.clear();
        }
        seg.push(p);
    }
    if seg.len() < 2 {
        return Ok(None);
    }
    let (first, last) = (seg[0], seg[seg.len() - 1]);
    let span = hours_between(last.0, first.0);
    if span < MIN_SPAN_H {
        return Ok(None);
    }
    Ok(Some(((last.1 - first.1) / span).max(0.0)))
}

/// Fallback rate: the account's 7d use over the hours its window has been open.
pub fn window_average(v: &Map<String, Value>, now: i64) -> Result<Option<f64>, Fault> {
    let reset = parse_ts(v.get("reset_7d"))?;
    let (Some(reset), Some(seven)) = (reset, py::num(v.get("7d"))) else { return Ok(None) };
    if py::truthy(v.get("stale")) {
        return Ok(None);
    }
    let opened = reset - 7 * 24 * HOUR_US;
    if opened < DT_MIN_US {
        return Err(overflow());
    }
    let open_h = hours_between(now, opened);
    Ok((MIN_SPAN_H..=(7 * 24) as f64).contains(&open_h).then(|| seven / open_h))
}

/// Which accounts decide the capacity gates: readiness()'s bounds, from the balancer's state.
#[derive(Debug, Clone, PartialEq)]
pub struct Gate {
    /// The balancer switches accounts on its own right now.
    pub failover: bool,
    pub stale_ok_min: f64,
    pub standby_max: f64,
    pub standby_weekly_max: f64,
    /// Accounts a probe found limited within the balancer's hour: no failover target.
    pub held: Vec<String>,
}

impl Default for Gate {
    /// No balancer state: failover never covers, so a stale active account fails closed.
    fn default() -> Gate {
        Gate {
            failover: false,
            stale_ok_min: STALE_OK_MIN,
            standby_max: STANDBY_MAX_5H,
            standby_weekly_max: LIMIT_7D,
            held: vec![],
        }
    }
}

/// Can the fleet spend this account? Not when it needs a login, or is limited: rate-limited, or at
/// `LIMIT_7D` of a 7d window not yet reset.
pub fn spendable(v: &Map<String, Value>, now: i64) -> Result<bool, Fault> {
    if py::truthy(v.get("needs_login")) || py::truthy(v.get("limited")) {
        return Ok(false);
    }
    let reset = parse_ts(v.get("reset_7d"))?;
    let limited = py::num(v.get("7d")).is_some_and(|s| s >= LIMIT_7D) && reset.is_none_or(|r| r > now);
    Ok(!limited)
}

/// `readiness()`: which accounts decide the capacity gates, from one sample's accounts.
///
/// A standby the fleet cannot spend, or without fresh numbers, never decides them. The active
/// account is never dropped: its fresh numbers count, and so do stale ones under `stale_ok_min`
/// old or still in the same 5h window (a 5h reading only rises until its reset); they can close
/// the gate, never open it. Stale, unreadable or unspendable, it is unknown and fails closed
/// unless failover covers it: a fresh standby passing the balancer's own standby test, picked by
/// the balancer's rank. `pool` is the accounts whose 7d headroom and relief the forecast counts.
pub fn readiness(accounts: &Accounts, now: i64, gate: &Gate) -> Result<Map<String, Value>, Fault> {
    let mut sorted: Vec<&(String, Map<String, Value>)> = accounts.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    let mut counted = Map::new();
    let mut unknown: Vec<String> = Vec::new();
    let mut standbys: Vec<(f64, f64, &str)> = Vec::new();
    let mut active: Option<&str> = None;
    for (name, v) in sorted.iter().map(|(n, v)| (n, v)) {
        let (five, seven) = (py::num(v.get("5h")), py::num(v.get("7d")));
        let stale = py::truthy(v.get("stale"));
        if py::truthy(v.get("active")) {
            active = Some(name);
            let reset5 = parse_ts(v.get("reset_5h"))?;
            let young = py::num(v.get("age_s")).is_some_and(|a| a < gate.stale_ok_min * 60.0);
            if five.is_some() && (!stale || young || reset5.is_some_and(|r| r > now)) {
                counted.insert(name.clone(), v["5h"].clone());
            }
            if stale || five.is_none() || !spendable(v, now)? {
                unknown.push(name.clone());
            }
        } else if spendable(v, now)?
            && let Some(f) = five
            && !stale
        {
            counted.insert(name.clone(), v["5h"].clone());
            if f <= gate.standby_max
                && !seven.is_some_and(|s| s >= gate.standby_weekly_max)
                && !gate.held.contains(name)
            {
                standbys.push((f, seven.unwrap_or(0.0), name));
            }
        }
    }
    // The balancer's rank: lowest 5h, then lowest 7d, then name.
    standbys.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)).then(a.2.cmp(b.2)));
    let covered_by = (!unknown.is_empty() && gate.failover).then(|| standbys.first().map(|s| s.2)).flatten();
    // The fleet's 7d headroom and reset: the active account and every fresh standby, never one that
    // needs a login (no reset brings it back) nor a stale standby (the balancer never moves to it).
    let mut pool: Vec<&str> = sorted
        .iter()
        .filter(|(_, v)| {
            !py::truthy(v.get("needs_login")) && (py::truthy(v.get("active")) || !py::truthy(v.get("stale")))
        })
        .map(|(n, _)| n.as_str())
        .collect();
    pool.sort();
    let known = unknown.is_empty() || covered_by.is_some();
    let mut out = Map::new();
    out.insert("active".into(), json!(active));
    out.insert("pool".into(), json!(pool));
    out.insert("counted".into(), Value::Object(counted));
    out.insert("unknown".into(), json!(unknown));
    out.insert("covered_by".into(), json!(covered_by));
    out.insert("known".into(), json!(known));
    Ok(out)
}

/// `float(x)` on a loaded value: a number, a bool, or a string Python's float reads.
/// `float(v)` on a value the schema holds to a number.
fn py_float(v: &Value) -> f64 {
    v.as_f64().unwrap_or(f64::NAN)
}

/// `x or {}` then `.get`: the schema holds `x` to a mapping or null.
fn as_dict<'a>(v: Option<&'a Value>, empty: &'a Map<String, Value>) -> &'a Map<String, Value> {
    v.and_then(Value::as_object).unwrap_or(empty)
}

/// `float(p.get("at") or 0)`.
fn at_seconds(p: &Map<String, Value>) -> f64 {
    py::or(p.get("at"), None).map_or(0.0, py_float)
}

/// `gate_bounds()`: readiness()'s bounds from `provider_manager.py --json state` and the usage
/// rows (to name the accounts a probe holds). Failover counts only while the balancer switches on
/// its own: the version gate verified, autoBalance on, no hold in force, and no StopFailure report
/// pending. A standby a probe found limited within the balancer's hour is no failover target.
pub fn gate_bounds(state: &Map<String, Value>, now: i64, rows: &[Value]) -> Result<Gate, Fault> {
    crate::schema::state(state)?;
    crate::schema::usage(rows)?;
    let empty = Map::new();
    let now_s = now as f64 / 1e6;
    let cfg = as_dict(state.get("config"), &empty);
    let hold = parse_ts(py::or(state.get("holdUntil"), None))?;
    // A pending StopFailure report the balancer has not resolved: its switch has not happened.
    let signals = state.get("pendingSignals").and_then(Value::as_array).map_or(&[][..], Vec::as_slice);
    let pending = signals.iter().filter_map(Value::as_object).any(|p| now_s - at_seconds(p) < SIGNAL_TTL_S);
    let switching = as_dict(state.get("versionGate"), &empty).get("automaticSwitching") == Some(&json!("on"));
    let failover =
        switching && cfg.get("autoBalance") == Some(&Value::Bool(true)) && !hold.is_some_and(|h| h > now) && !pending;
    // id -> account name, a repeated id keeping its last row (a dict).
    let mut names: Vec<(Option<&str>, String)> = Vec::new();
    for a in rows {
        let id = a.get("id").and_then(Value::as_str);
        let name = py::str_of(a.get("email").unwrap_or(&json!("?")));
        let name = name.split('@').next().unwrap_or("").to_string();
        match names.iter_mut().find(|(i, _)| *i == id) {
            Some(slot) => slot.1 = name,
            None => names.push((id, name)),
        }
    }
    let mut held = Vec::new();
    for (i, p) in as_dict(state.get("probes"), &empty) {
        let Some((_, name)) = names.iter().find(|(id, _)| *id == Some(i.as_str())) else { continue };
        let p = p.as_object().expect("the schema holds a probe to a mapping");
        if p.get("result") == Some(&json!("limited")) && now_s - at_seconds(p) < PROBE_HOLD_S {
            held.push(name.clone());
        }
    }
    held.sort();
    let bound = |k: &str, default: f64| cfg.get(k).map_or(default, py_float);
    Ok(Gate {
        failover,
        stale_ok_min: bound("staleNumbersMinutes", STALE_OK_MIN),
        standby_max: bound("standbyMax", STANDBY_MAX_5H),
        standby_weekly_max: bound("standbyWeeklyMax", LIMIT_7D),
        held,
    })
}

/// `resume_gate()`, the snapshot's `usage.gate.resume`: every account that decides it is under
/// `RESUME_5H` of 5h, and an unknown active account is covered by failover. No account to decide
/// it fails closed.
pub fn resume_gate(accounts: &Accounts, now: i64, gate: &Gate) -> Result<Value, Fault> {
    let r = readiness(accounts, now, gate)?;
    let five = r["counted"].as_object().cloned().unwrap_or_default();
    let joined =
        |k: &str| r[k].as_array().map(|a| a.iter().map(py::str_of).collect::<Vec<_>>().join(", ")).unwrap_or_default();
    // The first account at the maximum, in name order, as Python's max() picks it.
    let top = five.iter().fold(None::<(&String, &Value, f64)>, |best, (n, v)| {
        let f = py::num(Some(v)).unwrap_or(f64::NAN);
        match best {
            Some((_, _, b)) if f <= b => best,
            _ => Some((n, v, f)),
        }
    });
    let (ok, why) = if r["active"].is_null() {
        // An empty store: the balancer never logs back in on its own, so nothing resumes.
        (false, "no active account".to_string())
    } else if r["known"] != json!(true) {
        (false, format!("{} usage stale, unreadable or unspendable, no failover standby", joined("unknown")))
    } else if let Some((name, _, max)) = top.filter(|t| t.2 >= RESUME_5H) {
        (false, format!("5h {}% >= {}% on {name}", py::fmt_g(max), RESUME_5H as i64))
    } else if top.is_none() {
        (false, "no spendable account with readable usage".to_string())
    } else {
        let mut why =
            format!("5h under {}% on {}", RESUME_5H as i64, five.keys().cloned().collect::<Vec<_>>().join(", "));
        if let Some(c) = r["covered_by"].as_str().filter(|c| !c.is_empty()) {
            why += &format!("; {} stale, covered by failover to {c}", joined("unknown"));
        }
        (true, why)
    };
    let mut out = Map::new();
    out.insert("resume".into(), json!(ok));
    out.insert("why".into(), json!(why));
    out.extend(r);
    Ok(Value::Object(out))
}

fn usable(v: &Map<String, Value>) -> Option<f64> {
    py::num(v.get("7d")).filter(|s| !py::truthy(v.get("limited")) && *s < 100.0)
}

/// The fleet's runway against its next relief, from the latest sample and the history. `known` is
/// `readiness()`'s: only an account the fleet can spend decides it, and capacity and relief come
/// only from its pool (BRO-3223).
pub fn forecast(current: &Accounts, samples: &[Value], now: i64, gate: &Gate) -> Result<Map<String, Value>, Fault> {
    crate::schema::samples(samples)?;
    let mut rates: Vec<f64> = Vec::new();
    let mut headroom = 0.0;
    let mut per = Map::new();
    let ready = readiness(current, now, gate)?;
    let known = ready["known"] == json!(true);
    let pool: Vec<String> = ready["pool"].as_array().map(|a| a.iter().map(py::str_of).collect()).unwrap_or_default();
    let (mut measured_basis, mut average_basis) = (false, false);
    for (name, v) in current {
        let seven = v.get("7d").cloned().unwrap_or(Value::Null);
        let ok = usable(v).filter(|_| pool.contains(name));
        let (r, basis) = match burn_rate(samples, name, now)? {
            Some(r) => (Some(r), "measured"),
            None => (window_average(v, now)?, "window_average"),
        };
        if let (Some(r), Some(_)) = (r, ok) {
            // A limited account burns nothing now: only usable ones count.
            rates.push(r);
            if basis == "measured" {
                measured_basis = true;
            } else {
                average_basis = true;
            }
        }
        if let Some(s) = ok {
            headroom += 100.0 - s;
        }
        let reset = parse_ts(v.get("reset_7d"))?;
        let raw = |k: &str| v.get(k).cloned().unwrap_or(Value::Null);
        let mut row = json!({
            "5h": raw("5h"), "7d": seven, "reset_5h": raw("reset_5h"), "reset_7d": iso(reset),
            "active": raw("active"), "limited": raw("limited"),
            "burn_pct_h": r.map(|r| py::round(r, 2)), "burn_basis": r.map(|_| basis),
        });
        if let (Some(r), Some(s)) = (r, ok)
            && r != 0.0
        {
            row["exhausts_at"] = iso(Some(add_hours(now, (100.0 - s) / r)?));
        }
        per.insert(name.clone(), row);
    }
    let mut relief: Option<i64> = None;
    for (_, v) in current.iter().filter(|(n, _)| pool.contains(n)) {
        if let Some(r) = parse_ts(v.get("reset_7d"))?.filter(|r| *r > now) {
            relief = Some(relief.map_or(r, |m: i64| m.min(r)));
        }
    }
    let fleet_rate = (!rates.is_empty()).then(|| rates.iter().fold(0.0, |a, b| a + b));
    let mut out = Map::new();
    out.insert("accounts".into(), Value::Object(per));
    out.insert("headroom_pct".into(), json!(py::round(headroom, 1)));
    out.insert("relief_at".into(), iso(relief));
    out.insert("burn_pct_h".into(), json!(fleet_rate.map(|f| py::round(f, 2))));
    out.insert("known".into(), json!(known));
    out.insert("pool".into(), json!(pool));
    let mut set = |k: &str, v: Value| {
        out.insert(k.into(), v);
    };
    match fleet_rate {
        None => {
            set("runway_h", Value::Null);
            set("exhausts_at", Value::Null);
            set("crosses_relief", Value::Null);
            set("basis", json!("insufficient_samples"));
        }
        Some(f) if f <= 0.0 => {
            set("runway_h", Value::Null);
            set("exhausts_at", Value::Null);
            set("crosses_relief", json!(false));
            set("basis", json!("no_burn"));
        }
        Some(f) => {
            let runway = headroom / f;
            let ex = add_hours(now, runway)?;
            set("runway_h", json!(py::round(runway, 1)));
            set("exhausts_at", iso(Some(ex)));
            // Unknown relief fails closed.
            let crosses = relief.is_none_or(|rl| ex < rl);
            set("crosses_relief", json!(crosses));
            set("basis", json!(if measured_basis && !average_basis { "measured" } else { "window_average" }));
        }
    }
    Ok(out)
}

/// The tick period and the launch policy, from the most severe rung that applies.
pub fn cadence(current: &Accounts, fc: &Map<String, Value>) -> Value {
    let active = current.iter().find(|(_, v)| py::truthy(v.get("active")));
    let a5 = active.and_then(|(_, v)| py::num(v.get("5h")));
    let pool = fc.get("pool").and_then(Value::as_array);
    let in_pool = |n: &str| pool.is_some_and(|p| p.iter().any(|x| x.as_str() == Some(n)));
    let usable7: Vec<f64> = current.iter().filter(|(n, _)| in_pool(n)).filter_map(|(_, v)| usable(v)).collect();
    let crosses = py::truthy(fc.get("crosses_relief"));
    let runway = py::num(py::or(fc.get("runway_h"), None)).unwrap_or(0.0);
    let show = |k: &str| fc.get(k).map_or_else(|| "None".to_string(), py::str_of);
    let (mode, why) = if let Some(a5) = a5.filter(|a| *a >= READ_ONLY_5H) {
        (Mode::ReadOnly, format!("active 5h {}% >= {}%", py::fmt_g(a5), READ_ONLY_5H as i64))
    } else if usable7.is_empty() || usable7.iter().copied().fold(f64::INFINITY, f64::min) >= CRITICAL_7D {
        (Mode::Critical, format!("no account under {}% of 7d", CRITICAL_7D as i64))
    } else if crosses && runway < CRITICAL_RUNWAY_H {
        (
            Mode::Critical,
            format!(
                "runway {}h < {}h, before relief {}",
                show("runway_h"),
                CRITICAL_RUNWAY_H as i64,
                show("relief_at")
            ),
        )
    } else if crosses {
        (Mode::Conserve, format!("exhausts {} before relief {}", show("exhausts_at"), show("relief_at")))
    } else if !py::truthy(fc.get("known")) {
        // Stale or unreadable usage fails closed.
        (Mode::Conserve, "usage stale or unreadable".to_string())
    } else {
        (Mode::Normal, format!("headroom ({})", show("basis")))
    };
    let (hours, cap) = mode.rung();
    json!({
        "mode": mode.as_str(), "hours": hours, "cron": format!("13 */{hours} * * *"), "max_driven": cap,
        "hold_launches": mode != Mode::Normal, "read_only": mode == Mode::ReadOnly, "why": why,
    })
}

/// The snapshot's `capacity` section. `samples` must already include the current one. `gate`:
/// readiness()'s failover and bounds; the default (no balancer state) fails a stale active closed.
pub fn capacity(usage_rows: &[Value], samples: &[Value], now: i64, gate: &Gate) -> Result<Value, Fault> {
    let (_, current) = sample_from_usage(usage_rows, now)?;
    let mut fc = forecast(&current, samples, now, gate)?;
    let cad = cadence(&current, &fc);
    let mut recent = 0;
    for s in samples {
        if let Some(at) = parse_ts(s.get("at"))?
            && (now - at) as f64 / 1e6 <= (WINDOW_H * 3600) as f64
        {
            recent += 1;
        }
    }
    fc.insert("cadence".into(), cad);
    fc.insert("samples_24h".into(), json!(recent));
    Ok(Value::Object(fc))
}

/// The resume gate for one session: a handoff candidate above `HANDOFF_K`, and a driven session
/// above `SUCCESSOR_K` gets a fresh successor instead of a resume. `context_k` none counts as 0.
pub fn session_gates(context_k: Option<i64>, driven: bool) -> (bool, bool) {
    let ctx = context_k.unwrap_or(0);
    (ctx > HANDOFF_K, driven && ctx > SUCCESSOR_K)
}

/// Tiers in the order Maestro's economy by tier lists them.
pub const TIER_ORDER: [&str; 5] = ["mechanical", "standard", "frontier", "apex", "unknown"];
/// A tier whose runs raise this often is assessed too low.
pub const REPLAN_RAISED: f64 = 0.3;
/// A tier whose approved work passes first time less often than this is too low.
pub const REPLAN_FIRST_PASS: f64 = 0.5;

/// A tier count in Python's arithmetic: the schema holds counts to JSON integers.
fn count(v: &Value) -> f64 {
    v.as_f64().unwrap_or(0.0)
}

/// Rows per tier from Maestro's economy.json, and the re-plan signals the tick acts on.
pub fn tier_signals(data: &Value) -> Result<(Vec<Value>, Vec<String>), Fault> {
    crate::schema::tiers(data)?;
    // `(data or {}).get("tiers") or {}`: the schema holds both to mappings or null.
    let empty = Value::Object(Map::new());
    let tiers = data.get("tiers").filter(|t| t.is_object()).unwrap_or(&empty);
    let (mut rows, mut signals) = (Vec::new(), Vec::new());
    for t in TIER_ORDER {
        let Some(Value::Object(v)) = tiers.get(t) else { continue };
        if !py::truthy(v.get("runs")) {
            continue;
        }
        let raw = |k: &str| v.get(k).cloned().unwrap_or(Value::Null);
        let runs = raw("runs");
        rows.push(json!({
            "tier": t, "runs": runs, "approved": raw("approved"), "tokens_per_approved": raw("tokensPerApproved"),
            "first_pass": raw("firstPass"), "raises": raw("raises"), "raised_runs": raw("raisedRuns"),
            "escalations": raw("escalations"), "unmeasured": raw("unmeasured"),
        }));
        // `raised / runs >= REPLAN_RAISED`: runs is truthy here, so never a zero divisor.
        let raised = py::or(v.get("raisedRuns"), None).cloned().unwrap_or(json!(0));
        if count(&raised) / count(&runs) >= REPLAN_RAISED {
            signals.push(format!(
                "{t}: {}/{} runs raised a tier — assess this work one rung higher",
                py::str_of(&raised),
                py::str_of(&runs)
            ));
        }
        // `_num(fp) and (approved or 0) >= 3 and fp < ...`: approved is compared only for a numeric fp.
        if let Some(fp) = py::num(v.get("firstPass")) {
            let approved = py::or(v.get("approved"), None).cloned().unwrap_or(json!(0));
            if count(&approved) >= 3.0 && fp < REPLAN_FIRST_PASS {
                signals.push(format!(
                    "{t}: first-pass {:.0}% over {} approved — briefs or tier too thin",
                    fp * 100.0,
                    py::str_of(&raw("approved"))
                ));
            }
        }
    }
    Ok((rows, signals))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> i64 {
        parse_ts(Some(&json!(s))).unwrap().unwrap()
    }

    #[test]
    fn timestamps_read_in_the_strict_utc_form_only() {
        assert_eq!(iso(Some(at("2026-10-22T12:00:00Z"))), json!("2026-10-22T12:00:00+00:00"));
        assert_eq!(iso(Some(at("2026-10-22T12:00:00+00:00"))), json!("2026-10-22T12:00:00+00:00"));
        assert_eq!(at("2026-10-22T12:00:00.5Z") - at("2026-10-22T12:00:00Z"), 500_000);
        assert_eq!(at("2026-10-22T12:00:00.123456+00:00") - at("2026-10-22T12:00:00Z"), 123_456);
        // datetime's whole range, both ends, the leap day.
        assert_eq!(iso(Some(at("0001-01-01T00:00:00Z"))), json!("0001-01-01T00:00:00+00:00"));
        assert_eq!(iso(Some(at("9999-12-31T23:59:59.999999Z"))), json!("9999-12-31T23:59:59+00:00"));
        assert_eq!(iso(Some(at("2024-02-29T00:00:00Z"))), json!("2024-02-29T00:00:00+00:00"));
        assert_eq!(
            parse_ts(Some(&json!(1_700_000_000_000i64))).unwrap(),
            parse_ts(Some(&json!(1_700_000_000))).unwrap()
        );
        for t in [
            "yesterday",
            "2026-10-22T07:00:00-05:00",
            "2026-10-22 12:00",
            "2026-10-22",
            "2026-10-22T24:00:00Z",
            "2026-02-29T00:00:00Z",
            "2026-10-22T12:00:00.Z",
            "2026-10-22T12:00:00.1234567Z",
            " 2026-10-22T12:00:00Z",
        ] {
            assert!(matches!(parse_ts(Some(&json!(t))), Err(Fault::Refused { .. })), "{t}");
        }
        // Above 1e11 a number is milliseconds: 253402300800000 ms is year 10000.
        for n in [json!(253_402_300_800_000i64), json!(-62_135_596_801i64), json!(1e300), json!(-1e12)] {
            assert!(matches!(parse_ts(Some(&n)), Err(Fault::Refused { .. })), "{n}");
        }
        assert_eq!(parse_ts(Some(&json!(""))).unwrap(), None);
    }

    #[test]
    fn the_session_gates_are_strict() {
        assert_eq!(session_gates(Some(400), true), (false, false));
        assert_eq!(session_gates(Some(401), false), (true, false));
        assert_eq!(session_gates(Some(601), true), (true, true));
        assert_eq!(session_gates(None, true), (false, false));
    }
}
