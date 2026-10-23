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

const US: i64 = 1_000_000;
const HOUR_US: i64 = 3600 * US;

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

/// `parse_ts`: none for null or `""`; a number is epoch seconds, or milliseconds above 1e11;
/// a string is ISO 8601 as `datetime.fromisoformat` reads it, naive meaning UTC.
pub fn parse_ts(v: Option<&Value>) -> Result<Option<i64>, String> {
    match v {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if s.is_empty() => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(from_seconds(*b as i64 as f64))),
        Some(Value::Number(n)) => {
            let f = n.as_f64().unwrap_or(f64::NAN);
            Ok(Some(from_seconds(f / if f > 1e11 { 1000.0 } else { 1.0 })))
        }
        Some(other) => {
            let s = py::str_of(other);
            let t = s.trim().replace('Z', "+00:00");
            from_iso(&t).map(Some)
        }
    }
}

/// `datetime.fromtimestamp(f, utc)`: whole seconds, then the fraction rounded half-even to µs.
fn from_seconds(f: f64) -> i64 {
    let whole = f.trunc();
    let us = round_half_even((f - whole) * 1e6);
    whole as i64 * US + us
}

fn round_half_even(f: f64) -> i64 {
    let r = f.round();
    let r = if (f - f.trunc()).abs() == 0.5 { 2.0 * (f / 2.0).round() } else { r };
    r as i64
}

/// `datetime + timedelta(hours=h)` as CPython's `timedelta_new` builds it: the whole hours exactly,
/// the fraction times 3.6e9 µs in double precision, rounded half to even.
pub fn add_hours(at: i64, hours: f64) -> Option<i64> {
    if !hours.is_finite() || hours.abs() > 1e8 {
        return None;
    }
    let whole = hours.trunc();
    Some(at + whole as i64 * HOUR_US + round_half_even((hours - whole) * HOUR_US as f64))
}

fn digits(s: &str) -> Option<i64> {
    (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())).then(|| s.parse().ok()).flatten()
}

/// The subset of `datetime.fromisoformat` the usage endpoint and the samples write:
/// `YYYY-MM-DD[?HH[:MM[:SS[.f]]][±HH[:MM[:SS]]]]`, any one separator character. A field out of
/// range carries the Python's message; any other shape is `Invalid isoformat string`.
fn from_iso(t: &str) -> Result<i64, String> {
    let invalid = || format!("Invalid isoformat string: {}", py::repr_str(t));
    let chars: Vec<char> = t.chars().collect();
    if chars.len() < 10 || chars[4] != '-' || chars[7] != '-' {
        return Err(invalid());
    }
    let s = |a: usize, b: usize| chars[a..b].iter().collect::<String>();
    let field = |a, b| digits(&s(a, b)).ok_or_else(invalid);
    let (y, m, d) = (field(0, 4)?, field(5, 7)?, field(8, 10)?);
    let two = |p: Option<&str>| p.filter(|p| p.len() == 2).and_then(digits).ok_or_else(invalid);
    let (mut hh, mut mm, mut ss, mut us, mut off) = (0, 0, 0, 0i64, 0i64);
    if chars.len() > 10 {
        let rest: String = chars[11..].iter().collect();
        let split = rest.find(['+', '-']).unwrap_or(rest.len());
        let (time, tz) = rest.split_at(split);
        let mut parts = time.splitn(3, ':');
        hh = two(parts.next())?;
        if let Some(p) = parts.next() {
            mm = two(Some(p))?;
        }
        if let Some(p) = parts.next() {
            let (sec, frac) = p.split_once(['.', ',']).map_or((p, None), |(a, b)| (a, Some(b)));
            ss = two(Some(sec))?;
            if let Some(frac) = frac {
                digits(frac).ok_or_else(invalid)?;
                let six: String = frac.chars().chain(std::iter::repeat('0')).take(6).collect();
                us = digits(&six).ok_or_else(invalid)?;
            }
        }
        if !tz.is_empty() {
            let sign = if tz.starts_with('-') { -1 } else { 1 };
            let mut f = tz[1..].splitn(3, ':');
            let oh = two(f.next())?;
            let om = f.next().map_or(Ok(0), |p| two(Some(p)))?;
            let os = f.next().map_or(Ok(0), |p| two(Some(p)))?;
            off = sign * (oh * 3600 + om * 60 + os);
            if off.abs() >= 86_400 {
                return Err(invalid());
            }
        }
    }
    // The order and wording of datetime's own range checks.
    if !(1..=9999).contains(&y) {
        return Err(format!("year must be in 1..9999, not {y}"));
    }
    if !(1..=12).contains(&m) {
        return Err(format!("month must be in 1..12, not {m}"));
    }
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let dim = [31, if leap { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31][m as usize - 1];
    if !(1..=dim).contains(&d) {
        return Err(format!("day {d} must be in range 1..{dim} for month {m} in year {y}"));
    }
    for (name, v, max) in [("hour", hh, 23), ("minute", mm, 59), ("second", ss, 59)] {
        if v > max {
            return Err(format!("{name} must be in 0..{max}, not {v}"));
        }
    }
    let date = jiff::civil::Date::new(y as i16, m as i8, d as i8).map_err(|_| invalid())?;
    let utc = date
        .at(hh as i8, mm as i8, ss as i8, 0)
        .to_zoned(jiff::tz::TimeZone::UTC)
        .map_err(|_| invalid())?
        .timestamp()
        .as_second();
    Ok((utc - off) * US + us)
}

/// `iso(d)`: UTC, whole seconds, `+00:00`.
pub fn iso(us: Option<i64>) -> Value {
    let Some(us) = us else { return Value::Null };
    let secs = us.div_euclid(US);
    match jiff::Timestamp::from_second(secs) {
        Ok(ts) => {
            let dt = ts.to_zoned(jiff::tz::TimeZone::UTC).datetime();
            Value::String(format!(
                "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}+00:00",
                dt.year(),
                dt.month(),
                dt.day(),
                dt.hour(),
                dt.minute(),
                dt.second()
            ))
        }
        Err(_) => Value::Null,
    }
}

fn hours_between(later: i64, earlier: i64) -> f64 {
    (later - earlier) as f64 / 1e6 / 3600.0
}

/// One account's line in a sample, in the order the usage rows gave them.
pub type Accounts = Vec<(String, Map<String, Value>)>;

/// `sample_from_usage`: one cache line from the usage rows, per account (email local part).
/// A repeated name keeps its first position and its last row, as a Python dict does.
pub fn sample_from_usage(rows: &[Value], now: i64) -> (Value, Accounts) {
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
        match accounts.iter_mut().find(|(n, _)| *n == name) {
            Some(slot) => slot.1 = v,
            None => accounts.push((name, v)),
        }
    }
    let map: Map<String, Value> = accounts.iter().map(|(n, v)| (n.clone(), Value::Object(v.clone()))).collect();
    (json!({"at": iso(Some(now)), "accounts": map}), accounts)
}

/// Two 7d reset stamps name the same window (the endpoint jitters by sub-seconds).
fn same_window(a: Option<&Value>, b: Option<&Value>) -> Result<bool, String> {
    Ok(match (parse_ts(a)?, parse_ts(b)?) {
        (Some(x), Some(y)) => ((x - y) as f64 / 1e6).abs() < 3600.0,
        (x, y) => x.is_none() && y.is_none(),
    })
}

/// 7d percentage points per hour for one account over the last `WINDOW_H`, or none. Only the
/// current window counts: a different reset stamp, or a utilization drop, starts a new segment.
pub fn burn_rate(samples: &[Value], account: &str, now: i64) -> Result<Option<f64>, String> {
    let mut pts: Vec<(i64, f64, Option<Value>)> = Vec::new();
    for s in samples {
        let at = parse_ts(s.get("at"))?;
        let accts = py::or(s.get("accounts"), None);
        let v = accts.and_then(|a| py::or(a.get(account), None));
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
pub fn window_average(v: &Map<String, Value>, now: i64) -> Result<Option<f64>, String> {
    let reset = parse_ts(v.get("reset_7d"))?;
    let (Some(reset), Some(seven)) = (reset, py::num(v.get("7d"))) else { return Ok(None) };
    if py::truthy(v.get("stale")) {
        return Ok(None);
    }
    let open_h = hours_between(now, reset - 7 * 24 * HOUR_US);
    Ok((MIN_SPAN_H..=(7 * 24) as f64).contains(&open_h).then(|| seven / open_h))
}

fn usable(v: &Map<String, Value>) -> Option<f64> {
    py::num(v.get("7d")).filter(|s| !py::truthy(v.get("limited")) && *s < 100.0)
}

/// The fleet's runway against its next relief, from the latest sample and the history.
pub fn forecast(current: &Accounts, samples: &[Value], now: i64) -> Result<Map<String, Value>, String> {
    let mut rates: Vec<f64> = Vec::new();
    let mut headroom = 0.0;
    let mut per = Map::new();
    let mut known = true;
    let (mut measured_basis, mut average_basis) = (false, false);
    for (name, v) in current {
        let seven = v.get("7d").cloned().unwrap_or(Value::Null);
        let ok = usable(v);
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
        if py::num(Some(&seven)).is_none() || py::truthy(v.get("stale")) {
            known = false;
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
            row["exhausts_at"] = iso(add_hours(now, (100.0 - s) / r));
        }
        per.insert(name.clone(), row);
    }
    let mut relief: Option<i64> = None;
    for (_, v) in current {
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
            let ex = add_hours(now, runway);
            set("runway_h", json!(py::round(runway, 1)));
            set("exhausts_at", iso(ex));
            // Unknown relief fails closed.
            let crosses = match (relief, ex) {
                (None, _) => true,
                (Some(rl), Some(ex)) => ex < rl,
                (Some(_), None) => false,
            };
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
    let usable7: Vec<f64> = current.iter().filter_map(|(_, v)| usable(v)).collect();
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

/// The snapshot's `capacity` section. `samples` must already include the current one.
pub fn capacity(usage_rows: &[Value], samples: &[Value], now: i64) -> Result<Value, String> {
    let (_, current) = sample_from_usage(usage_rows, now);
    let mut fc = forecast(&current, samples, now)?;
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

/// Rows per tier from Maestro's economy.json, and the re-plan signals the tick acts on.
pub fn tier_signals(data: &Value) -> (Vec<Value>, Vec<String>) {
    let tiers = py::or(data.get("tiers"), None);
    let (mut rows, mut signals) = (Vec::new(), Vec::new());
    for t in TIER_ORDER {
        let Some(Value::Object(v)) = tiers.and_then(|x| x.get(t)) else { continue };
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
        let raised = py::or(v.get("raisedRuns"), None).cloned().unwrap_or(json!(0));
        if let (Some(rn), Some(rd)) = (py::num(Some(&runs)), py::num(Some(&raised)))
            && rn != 0.0
            && rd / rn >= REPLAN_RAISED
        {
            signals.push(format!(
                "{t}: {}/{} runs raised a tier — assess this work one rung higher",
                py::str_of(&raised),
                py::str_of(&runs)
            ));
        }
        let approved = py::or(v.get("approved"), None).and_then(|a| py::num(Some(a))).unwrap_or(0.0);
        if let Some(fp) = py::num(v.get("firstPass"))
            && approved >= 3.0
            && fp < REPLAN_FIRST_PASS
        {
            signals.push(format!(
                "{t}: first-pass {:.0}% over {} approved — briefs or tier too thin",
                fp * 100.0,
                py::str_of(&raw("approved"))
            ));
        }
    }
    (rows, signals)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> i64 {
        parse_ts(Some(&json!(s))).unwrap().unwrap()
    }

    #[test]
    fn timestamps_read_as_python_reads_them() {
        assert_eq!(iso(Some(at("2026-10-22T12:00:00Z"))), json!("2026-10-22T12:00:00+00:00"));
        assert_eq!(iso(Some(at("2026-10-22T07:00:00-05:00"))), json!("2026-10-22T12:00:00+00:00"));
        assert_eq!(iso(Some(at("2026-10-22 12:00"))), json!("2026-10-22T12:00:00+00:00"));
        assert_eq!(at("2026-10-22T12:00:00.5Z") - at("2026-10-22T12:00:00Z"), 500_000);
        assert_eq!(
            parse_ts(Some(&json!(1_700_000_000_000i64))).unwrap(),
            parse_ts(Some(&json!(1_700_000_000))).unwrap()
        );
        assert!(parse_ts(Some(&json!("yesterday"))).is_err());
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
