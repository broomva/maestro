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
    let degraded =
        [&available(&census), &available(&capacity), &available(&history), &available(&loops), &available(&log)]
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
                        span { (chip(if census.is_degraded() { "unknown" } else { "ok" })) " Hosts" }
                        span { (chip(if census.is_degraded() { "unknown" } else if signals.is_some_and(|s| !s.is_empty()) { "warn" } else { "ok" })) " Providers" }
                        span { (chip(if capacity.is_degraded() { "unknown" } else { "ok" })) " Tokens" }
                        span { (chip(if loops.is_degraded() { "unknown" } else if has_problems { "warn" } else { "ok" })) " Loops" }
                        span { (chip(if degraded { "unknown" } else { "ok" })) " Sources" }
                    }
                }
                div class="mh-attention" { strong { "Needs a look" }
                    @if let Some(signals) = signals { @for signal in signals { span { (word(signal,"host")) " · " (word(signal,"instance")) } } }
                    @if degraded { span { "Some information could not be read" } }
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
                        article class="mh-token" { div { strong { (provider) } small { (account_label(&account)) } } div class="mh-token-detail" {
                            @if let Some(n) = used { div class="mh-track" { span style=(format!("width:{:.1}%", n.clamp(0.0,100.0))) {} } strong { (format!("{n:.0}% used")) } } @else { div class="mh-track mh-empty" {} span { "— not measured" } }
                            p { @if let Some(r) = routing { @if let Some((live, max)) = number(r,"live").zip(number(r,"max_concurrent")) { (format!("{live:.0} of {max:.0} slots")) } @else { "Slots not measured" } } @else { "Slots not measured" } " · " @if let Some(reset) = reset_time(lane) { "Resets " (reset) } @else { "Reset not measured" } }
                            p { @if history.is_degraded() { "History unavailable" } @else if samples.is_empty() { "History not measured" } @else { (samples.len()) " history samples" } " · " @if let Some(n) = number(lane,"hours_to_dry") { (format!("Estimated {n:.1} hours to empty")) } @else { "Projection not measured" } }
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
                @else { p { "Incident history is available. Exact intervals are not measured." } }
                div class="mh-hatched" aria-label="Incident intervals not measured" {}
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
    fn account_names_and_reset_shapes() {
        assert_eq!(account_label("team@example.com"), "team");
        assert_eq!(account_label("person@example.com"), "personal");
        assert_eq!(reset_time(&json!({"resets": {"weekly": "tomorrow"}})), Some("tomorrow"));
        assert_eq!(reset_time(&json!({"resets": ["later"]})), Some("later"));
        assert_eq!(reset_time(&json!({})), None);
    }
}
