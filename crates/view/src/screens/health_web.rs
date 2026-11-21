//! The read-only fleet health page.
use crate::render::Ctx;
use maestro_core::Section;
use maud::{Markup, html};
use serde_json::Value;
use std::collections::BTreeSet;

fn word(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or("not measured").to_owned()
}
fn number(v: &Value, key: &str) -> Option<f64> {
    v.get(key).and_then(Value::as_f64).filter(|n| n.is_finite())
}
fn metric(label: &str, n: Option<f64>, suffix: &str) -> Markup {
    html! { div class="mh-metric" { div class="mh-gauge" data-measured=(n.is_some()) { @if let Some(n) = n { (format!("{n:.0}{suffix}")) } @else { "—" } } strong { (label) } small { @if n.is_some() { "measured" } @else { "not measured" } } } }
}
fn state(s: &str) -> (&'static str, &'static str, &'static str) {
    match s {
        "up" | "ok" | "green" => ("ok", "✓", "Up"),
        "down" | "red" | "breach" => ("fail", "!", "Down"),
        "degraded" | "warn" | "amber" => ("warn", "!", "Needs a look"),
        "idle" => ("idle", "○", "Idle"),
        "disabled" => ("idle", "−", "Off"),
        _ => ("unknown", "−", "Not measured"),
    }
}
fn chip(s: &str) -> Markup {
    let (class, glyph, label) = state(s);
    html! { span class="mh-chip" data-state=(class) { span aria-hidden="true" { (glyph) } " " (label) } }
}
fn available<T>(s: &Section<T>) -> &'static str {
    if s.is_degraded() { "unavailable" } else { "read" }
}

fn freshness(age: Option<u64>, cadence: u64) -> Option<String> {
    match age {
        Some(age) if age > cadence => Some(format!("! Stale, read {} minutes ago", age / 60)),
        None => Some("— Source age not measured".into()),
        _ => None,
    }
}

fn source_age(cx: &Ctx, file: &str) -> Option<u64> {
    let modified = std::fs::metadata(cx.store.paths.fleet_meta.join(file)).ok()?.modified().ok()?;
    let seconds = modified.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
    Some((cx.at.as_second().max(0) as u64).saturating_sub(seconds))
}

fn account_label(account: &str) -> &str {
    if account.starts_with("devteam@") {
        "devteam"
    } else if account.starts_with("team@") {
        "team"
    } else if account.contains('@') {
        "personal"
    } else {
        account
    }
}

fn reset_time(lane: &Value) -> Option<&str> {
    let resets = lane.get("resets")?;
    resets
        .as_array()
        .and_then(|r| r.iter().find_map(Value::as_str))
        .or_else(|| resets.get("weekly").and_then(Value::as_str))
}

// Keep unread samples as gaps. A falling counter starts a new reset period.
fn history_points(samples: &[&maestro_store::health::Sample], now: i64) -> Vec<(i64, Option<f64>)> {
    let mut points: Vec<_> = samples
        .iter()
        .filter_map(|s| {
            let at = s.at.parse::<jiff::Timestamp>().ok()?.as_second();
            (at >= now - 86_400 && at <= now)
                .then_some((at, s.used.filter(|v| s.readable && v.is_finite() && (0.0..=100.0).contains(v))))
        })
        .collect();
    points.sort_by_key(|p| p.0);
    points
}

fn history_label(points: &[(i64, Option<f64>)]) -> &'static str {
    if points.iter().all(|(_, value)| value.is_none()) {
        "— History not measured"
    } else if points.iter().any(|(_, value)| value.is_none()) {
        "! History partially unreadable"
    } else {
        "24 hour history"
    }
}

fn projection(points: &[(i64, Option<f64>)], reset: Option<&str>, now: i64) -> Option<String> {
    let reset = reset?.parse::<jiff::Timestamp>().ok()?.as_second();
    if reset <= now {
        return None;
    }
    let &(last_at, Some(last)) = points.last()? else { return None };
    if now - last_at > 1800 {
        return None;
    }
    let mut first = (last_at, last);
    for &(at, value) in points.iter().rev().skip(1) {
        let Some(value) = value else { break };
        if value > first.1 {
            break;
        }
        first = (at, value);
    }
    let elapsed = last_at - first.0;
    let increase = last - first.1;
    if elapsed <= 0 || increase <= 0.0 {
        return None;
    }
    let seconds = ((80.0 - last).max(0.0) / increase) * elapsed as f64;
    let remaining = (last_at as f64 + seconds - now as f64).max(0.0);
    if last_at as f64 + seconds >= reset as f64 {
        Some("Reset before projected throttle".into())
    } else {
        Some(format!("Estimated {:.1} hours to throttle", remaining / 3600.0))
    }
}

fn sparkline(points: &[(i64, Option<f64>)], now: i64) -> Markup {
    let shade = points.first().map_or(100.0, |p| ((p.0 - (now - 86_400)) as f64 / 864.0).clamp(0.0, 100.0));
    let mut segments = vec![String::new()];
    for &(at, value) in points {
        if let Some(value) = value {
            use std::fmt::Write;
            let _ = write!(
                segments.last_mut().expect("one segment"),
                "{:.2},{:.2} ",
                (at - (now - 86_400)) as f64 / 864.0,
                100.0 - value
            );
        } else {
            segments.push(String::new());
        }
    }
    html! { div class="mh-spark" role="img" aria-label="Token usage over the last 24 hours; shaded time has no history" {
        div class="mh-spark-shade" style=(format!("width:{shade:.2}%")) {}
        svg viewBox="0 0 100 100" preserveAspectRatio="none" aria-hidden="true" {
            @for segment in segments { polyline points=(segment) {} }
        }
    } }
}

fn incidents(log: &str, now: i64) -> std::collections::BTreeMap<String, Vec<(i64, String)>> {
    let mut lanes = std::collections::BTreeMap::<String, Vec<(i64, String)>>::new();
    for line in log.lines() {
        let mut words = line.split_whitespace();
        let Some(at) = words.next().and_then(|s| s.parse::<jiff::Timestamp>().ok()) else { continue };
        let at = at.as_second();
        if at < now - 86_400 || at > now {
            continue;
        }
        let words: Vec<_> = words.collect();
        for pair in words.windows(2) {
            if let Some((_, to)) = pair[1].trim_end_matches(';').split_once("->") {
                let lane = pair[0];
                if lane.contains('/') {
                    lanes.entry(lane.to_owned()).or_default().push((at, to.to_owned()));
                }
            }
        }
    }
    for events in lanes.values_mut() {
        events.sort_by_key(|e| e.0);
    }
    lanes
}

fn timeline(log: &str, now: i64) -> Markup {
    let lanes = incidents(log, now);
    html! {
        p class="mh-note" { "Local time · shaded intervals are not measured" }
        @if lanes.is_empty() { p { "— No state transitions recorded in the last 24 hours" } }
        @for (lane, events) in lanes { div class="mh-incident" { strong { @if let Some((provider, account)) = lane.split_once('/') { (provider) " · " (account_label(account)) } }
            div class="mh-incident-track" {
                @for (i, (at, state)) in events.iter().enumerate() {
                    @let end = events.get(i+1).map_or(now, |e| e.0);
                    @let class = match state.as_str() { "down" | "red" => "fail", "degraded" | "amber" => "warn", "up" | "green" | "ok" => "ok", _ => "unknown" };
                    @let time = jiff::Timestamp::from_second(*at).ok().map(|t| t.to_zoned(jiff::tz::TimeZone::system()).strftime("%H:%M").to_string()).unwrap_or_default();
                    span class="mh-incident-segment" data-state=(class) style=(format!("left:{:.3}%;width:{:.3}%", (*at - (now-86_400)) as f64 / 864.0, (end-*at) as f64 / 864.0)) title=(format!("{} · {}", time, state)) {}
                }
            }
        } }
        p class="mh-note" { "! Down · ! Needs a look · — Not measured" }
    }
}

fn dispatch(e: &Value) -> Markup {
    let counts = number(e, "decisions_24h").zip(number(e, "denials_24h")).zip(number(e, "followed"));
    html! { div class="mh-dispatch" { h3 { "Dispatch gate · 24 hours" }
        @if let Some(((total, denied), followed)) = counts.filter(|((total, denied), followed)|
            *total >= *denied && *denied >= *followed && *followed >= 0.0) {
            p { (format!("✓ {:.0} allowed · ↪ {:.0} sent elsewhere · ! {:.0} denied", total-denied, followed, denied-followed)) }
        } @else { p { "— Not measured" } }
    } }
}

pub(crate) fn health(cx: &Ctx, id: &str) -> Markup {
    let census = cx.store.health_census();
    let capacity = cx.store.health_capacity();
    let history = cx.store.health_history();
    let loops = cx.store.health_loops();
    let log = cx.store.health_log();
    let sources = [
        ("Fleet census", "fleet-snapshot.json", 600),
        ("Capacity", "capacity.json", 1800),
        ("Token history", "capacity-history.jsonl", 1800),
        ("Loops", "loop-health.json", 600),
        ("Incident history", "capacity.log", 1800),
    ];
    let census_stale = freshness(source_age(cx, "fleet-snapshot.json"), 600).is_some();
    let capacity_stale = freshness(source_age(cx, "capacity.json"), 1800).is_some();
    let loops_stale = freshness(source_age(cx, "loop-health.json"), 600).is_some();
    let stale = sources.iter().any(|(_, file, cadence)| freshness(source_age(cx, file), *cadence).is_some());
    let degraded = stale
        || [&available(&census), &available(&capacity), &available(&history), &available(&loops), &available(&log)]
            .iter()
            .any(|s| **s != "read");
    let signals = census.data.as_ref().map(|c| &c.active_signals);
    let has_problems = signals.is_some_and(|s| !s.is_empty())
        || loops.data.as_ref().and_then(|l| l.loops.as_object()).is_some_and(|l| {
            l.values().any(|v| matches!(v.get("status").and_then(Value::as_str), Some("warn" | "breach")))
        });
    html! {
        div class="mh" data-block=(id) data-type="health" {
            section class="mh-verdict" aria-label="Fleet verdict" {
                div class="mh-verdict-main" {
                    div { h1 { @if degraded { "Some fleet information is unavailable." } @else if has_problems { "Some parts of the fleet need a look." } @else { "Fleet information is available." } }
                        p { "A current view of hosts, providers, capacity and the loops that keep work moving." }
                    }
                    div class="mh-pills" {
                        span { (chip(if census.is_degraded() || census_stale { "unknown" } else { "ok" })) " Hosts" }
                        span { (chip(if census.is_degraded() || census_stale { "unknown" } else if signals.is_some_and(|s| !s.is_empty()) { "warn" } else { "ok" })) " Providers" }
                        span { (chip(if capacity.is_degraded() || capacity_stale { "unknown" } else { "ok" })) " Tokens" }
                        span { (chip(if loops.is_degraded() || loops_stale { "unknown" } else if has_problems { "warn" } else { "ok" })) " Loops" }
                        span { (chip(if degraded { "unknown" } else { "ok" })) " Sources" }
                    }
                }
                div class="mh-attention" { strong { "Needs a look" }
                    @if let Some(signals) = signals { @for signal in signals { span { (word(signal,"host")) " · " (word(signal,"instance")) } } }
                    @if census.is_degraded() || capacity.is_degraded() || history.is_degraded() || loops.is_degraded() || log.is_degraded() { span { "! Some information is unavailable or partially read" } }
                    @for (label, file, cadence) in sources { @if let Some(message) = freshness(source_age(cx, file), cadence) { span { (label) ": " (message) } } }
                    @if !degraded && !has_problems { span { "No provider problems reported" } }
                }
            }
            div class="mh-upper" {
                section { h2 { "Hosts" } p class="mh-note" { "Tailnet latency: not measured" }
                    @if let Some(c) = &census.data {
                        div class="mh-hosts" { @for (key, label) in [("mac","Mac"),("nuc","NUC"),("vps","VPS")] {
                            @let host = &c.host_load[key];
                            article class="mh-host" { h3 { (label) } p { (host.get("cpus").and_then(Value::as_u64).map_or("Cores not measured".into(), |n| format!("{n} cores"))) }
                                p { (host.get("live").and_then(Value::as_u64).zip(c.host_caps.get(key).and_then(Value::as_u64)).map_or("Sessions not measured".into(), |(n, cap)| format!("{n} of {cap} sessions"))) }
                                div class="mh-metrics" {
                                    (metric("CPU load / core", number(host,"load_per_cpu").map(|n| n * 100.0), "%"))
                                    (metric("Memory", number(host,"mem_avail_pct").map(|n| 100.0-n), "%"))
                                    (metric("Swap", if key == "mac" { capacity.data.as_ref().and_then(|c| number(&c.host,"swap_used_mb").zip(number(&c.host,"swap_total_mb"))).and_then(|(u,t)| (t > 0.0).then_some(u/t*100.0)) } else { None }, "%"))
                                    (metric("Disk", None, "%")) (metric(if key == "mac" { "SSD" } else { "GPU" }, None, "%")) (metric("VRAM", None, "%"))
                                }
                                p class="mh-history-note" { "CPU and memory history: not measured" }
                            }
                        } }
                    } @else { p class="mh-unavailable" { "Host information is unavailable." } }
                }
                section { h2 { "Providers by host" }
                    @if let Some(c) = &census.data {
                        @let names: BTreeSet<_> = ["mac","nuc","vps"].into_iter().filter_map(|h| c.connections.get(h).and_then(Value::as_object)).flat_map(|v| v.keys().cloned()).collect();
                        div class="mh-table-wrap" { table class="mh-table" { thead { tr { th { "Provider" } th { "Mac" } th { "NUC" } th { "VPS" } } } tbody {
                            @for name in names { tr { th scope="row" { (name.clone()) } @for host in ["mac","nuc","vps"] { td { @if let Some(value) = c.connections.get(host).and_then(|h| h.get(&name)) { (chip(&word(value,"state"))) } @else { "—" } } } } }
                        } } }
                    } @else { p class="mh-unavailable" { "Provider information is unavailable." } }
                }
            }
            div class="mh-lower" {
                section { h2 { "Token capacity" } p class="mh-note" { "Usage against the 80% throttle · last 24 hours" }
                    @if let Some(c) = &capacity.data { @for lane in &c.lanes {
                        @let provider = word(lane,"provider"); @let account = word(lane,"account"); @let used = number(lane,"used");
                        @let routing = c.routing.iter().find(|r| word(r,"provider") == provider && word(r,"account") == account);
                        @let samples: Vec<_> = history.data.as_ref().into_iter().flatten().filter(|s| s.lane == format!("{provider}/{account}")).collect();
                        @let points = history_points(&samples, cx.at.as_second());
                        article class="mh-token" { div { strong { (provider) } small { (account_label(&account)) } } div class="mh-token-detail" {
                            @if let Some(n) = used { div class="mh-track" { span style=(format!("width:{:.1}%", n.clamp(0.0,100.0))) {} } strong { (format!("{n:.0}% used")) } } @else { div class="mh-track mh-empty" {} span { "— not measured" } }
                            p { @if let Some(r) = routing { @if let Some((live, max)) = number(r,"live").zip(number(r,"max_concurrent")) { (format!("{live:.0} of {max:.0} slots")) } @else { "Slots not measured" } } @else { "Slots not measured" } " · " @if let Some(reset) = reset_time(lane) { "Resets " (reset) } @else { "Reset not measured" } }
                            (sparkline(&points, cx.at.as_second()))
                            p { @if history.data.is_none() { "— History unavailable" } @else if history.is_degraded() { "! History partially read" } @else { (history_label(&points)) } " · " (if history.is_degraded() { None } else { projection(&points, reset_time(lane), cx.at.as_second()) }.unwrap_or_else(|| "— Projection not measured".into())) }
                        } }
                    } } @else { p class="mh-unavailable" { "Token capacity is unavailable." } }
                }
                section { h2 { "Loops" }
                    @if let Some(l) = &loops.data { @if let Some(map) = l.loops.as_object() { div class="mh-loops" { @for (name, value) in map { article { (chip(&word(value,"status"))) strong { (name.replace('_', " ")) } small { (word(value,"why")) } } } } }
                        @if let Some(e) = l.loops.get("dispatch_gate").and_then(|v| v.get("evidence")) { (dispatch(e)) }
                    }
                    @else { p class="mh-unavailable" { "Loop information is unavailable." } }
                }
            }
            section class="mh-timeline" { h2 { "Last 24 hours" }
                @if log.is_degraded() { p class="mh-unavailable" { "Incident history is unavailable." } }
                @else if let Some(text) = &log.data { (timeline(text, cx.at.as_second())) }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn dispatch_counts_require_consistent_evidence() {
        let good = dispatch(&json!({"decisions_24h": 10, "denials_24h": 3, "followed": 2})).into_string();
        assert!(good.contains("7 allowed"));
        assert!(good.contains("2 sent elsewhere"));
        assert!(good.contains("1 denied"));
        let bad = dispatch(&json!({"decisions_24h": 2, "denials_24h": 3, "followed": 1})).into_string();
        assert!(bad.contains("Not measured"));
    }

    #[test]
    fn incidents_use_transitions_inside_the_window() {
        let now = "2026-11-20T14:00:00Z".parse::<jiff::Timestamp>().unwrap().as_second();
        let rows = incidents(
            "2026-11-20T12:00:00Z codex/person@example.com green->red; codex/team@example.com unknown->down\n2026-11-20T13:00:00Z codex/person@example.com red->green",
            now,
        );
        assert_eq!(rows["codex/person@example.com"].len(), 2);
        assert_eq!(rows["codex/team@example.com"][0].1, "down");
        assert!(incidents("not a transition", now).is_empty());
    }

    #[test]
    fn history_label_does_not_call_unread_samples_measured() {
        assert_eq!(history_label(&[(1, None)]), "— History not measured");
        assert_eq!(history_label(&[(1, Some(1.0)), (2, None)]), "! History partially unreadable");
        assert_eq!(history_label(&[(1, Some(1.0))]), "24 hour history");
    }

    #[test]
    fn freshness_reports_old_or_unknown_sources() {
        assert_eq!(freshness(Some(601), 600).as_deref(), Some("! Stale, read 10 minutes ago"));
        assert!(freshness(Some(600), 600).is_none());
        assert!(freshness(None, 600).unwrap().contains("not measured"));
    }

    #[test]
    fn projection_stops_at_unread_and_reset_and_compares_reset_time() {
        let now = 1_700_000_000;
        let reset = jiff::Timestamp::from_second(now + 20_000).unwrap().to_string();
        assert!(
            projection(&[(now - 3600, Some(20.0)), (now, Some(40.0))], Some(&reset), now).unwrap().contains("hours")
        );
        let soon = jiff::Timestamp::from_second(now + 100).unwrap().to_string();
        assert!(
            projection(&[(now - 3600, Some(20.0)), (now, Some(40.0))], Some(&soon), now)
                .unwrap()
                .contains("Reset before")
        );
        assert!(
            projection(&[(now - 3600, Some(20.0)), (now - 10, None), (now, Some(40.0))], Some(&reset), now).is_none()
        );
        assert!(projection(&[(now - 3600, Some(90.0)), (now, Some(40.0))], Some(&reset), now).is_none());
        assert!(projection(&[(now - 7200, Some(20.0)), (now - 3600, Some(40.0))], Some(&reset), now).is_none());
        assert!(projection(&[(now - 3600, Some(20.0)), (now, Some(40.0))], None, now).is_none());
    }

    #[test]
    fn account_names_and_reset_shapes() {
        assert_eq!(account_label("team@example.com"), "team");
        assert_eq!(account_label("person@example.com"), "personal");
        assert_eq!(reset_time(&json!({"resets": {"weekly": "tomorrow"}})), Some("tomorrow"));
        assert_eq!(reset_time(&json!({"resets": ["later"]})), Some("later"));
        assert_eq!(reset_time(&json!({})), None);
    }
}
