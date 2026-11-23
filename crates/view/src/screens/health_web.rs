//! Operational fleet evidence. A successful file read is not proof of a healthy fleet.
use crate::render::Ctx;
use maestro_core::{Section, SourceError};
use maestro_store::health::{Capacity, Census, CensusSample, Sample, Signal};
use maud::{Markup, html};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

fn word<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("Not measured")
}
fn number(v: &Value, key: &str) -> Option<f64> {
    v.get(key).and_then(Value::as_f64).filter(|n| n.is_finite() && *n >= 0.0)
}
fn percent(v: &Value, key: &str) -> Option<f64> {
    number(v, key).filter(|n| *n <= 100.0)
}
fn has_values(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Object(v) => !v.is_empty(),
        Value::Array(v) => !v.is_empty(),
        _ => true,
    }
}
fn timestamp(s: &str) -> Option<i64> {
    s.parse::<jiff::Timestamp>().ok().map(|t| t.as_second())
}
fn local(at: i64) -> String {
    jiff::Timestamp::from_second(at)
        .ok()
        .map(|t| t.to_zoned(jiff::tz::TimeZone::system()).strftime("%b %d, %H:%M %Z").to_string())
        .unwrap_or_else(|| "Time unavailable".into())
}
fn sampled(at: Option<&str>, now: i64, cadence: u64) -> (bool, String) {
    match at.and_then(timestamp) {
        Some(t) if t <= now + 60 && now.saturating_sub(t) <= cadence as i64 => {
            (true, format!("Sampled {} · {} min ago", local(t), now.saturating_sub(t).max(0) / 60))
        }
        Some(t) if t <= now + 60 => (false, format!("Stale · sampled {} · {} min ago", local(t), (now - t) / 60)),
        _ => (false, "Sample time unavailable or ahead of this clock".into()),
    }
}
fn state(s: &str) -> (&'static str, &'static str) {
    match s {
        "up" | "ok" | "green" => ("ok", "Available"),
        "down" | "red" | "breach" | "failed" | "host_unreachable" => ("fail", "Needs attention"),
        "degraded" | "warn" | "amber" | "throttled" => ("warn", "Limited"),
        "idle" => ("idle", "Idle"),
        "disabled" | "off" | "inactive" => ("idle", "Off"),
        _ => ("unknown", "Not measured"),
    }
}
fn chip(s: &str) -> Markup {
    let (class, label) = state(s);
    html! { span class="mh-chip" data-state=(class) { (label) } }
}
fn problematic(s: &str) -> bool {
    matches!(state(s).0, "warn" | "fail")
}
fn hosts(c: &Census) -> Vec<String> {
    let names = [&c.host_load, &c.host_caps, &c.connections]
        .into_iter()
        .filter_map(Value::as_object)
        .flat_map(|m| m.keys().cloned())
        .collect::<BTreeSet<_>>();
    let mut ordered: Vec<_> = names.into_iter().collect();
    ordered.sort_by_key(|h| {
        (
            match h.as_str() {
                "mac" => 0,
                "nuc" => 1,
                "vps" => 2,
                _ => 3,
            },
            h.clone(),
        )
    });
    ordered
}
fn provider_scope_complete(c: &Census) -> bool {
    let registered = hosts(c);
    !registered.is_empty()
        && registered.iter().all(|host| {
            c.connections.get(host).and_then(Value::as_object).is_some_and(|connections| {
                !connections.is_empty() && connections.values().all(|v| state(word(v, "state")).0 != "unknown")
            })
        })
}
fn host_fresh(c: &Census, host: &str, now: i64) -> bool {
    sampled(c.host_last_observed.get(host).and_then(|v| v.get("at")).and_then(Value::as_str), now, 600).0
}
fn host_state<'a>(c: &'a Census, host: &str, fresh: bool) -> &'a str {
    if !fresh {
        return "unknown";
    }
    if !c.reachable_hosts.iter().any(|h| h == host) {
        return "down";
    }
    if has_values(&c.errors[host]) || has_values(&c.sensor_errors[host]) {
        return "degraded";
    }
    let h = &c.host_load[host];
    if number(h, "load_per_cpu").is_none() || percent(h, "mem_avail_pct").is_none() { "unknown" } else { "up" }
}
fn active_hold(capacity: &Capacity, host: &str) -> bool {
    capacity.host_holds.get(host).and_then(|v| v.get("active")).and_then(Value::as_bool) == Some(true)
}
fn lane_state(lane: &Value, fresh: bool) -> &str {
    if !fresh {
        return "unknown";
    }
    if percent(lane, "used").is_some_and(|n| n >= 80.0) {
        return "red";
    }
    let recorded = word(lane, "state");
    if state(recorded).0 == "ok" && percent(lane, "used").is_none() { "unknown" } else { recorded }
}
fn reset_time(lane: &Value) -> Option<&str> {
    let resets = lane.get("resets")?;
    resets
        .as_array()
        .and_then(|r| r.iter().filter_map(Value::as_str).min_by_key(|s| timestamp(s).unwrap_or(i64::MAX)))
        .or_else(|| resets.get("weekly").and_then(Value::as_str))
}
fn account_label(account: &str) -> &str {
    if account.starts_with("devteam@") {
        "Dev team"
    } else if account.starts_with("team@") {
        "Team"
    } else if account.contains('@') {
        "Personal"
    } else {
        account
    }
}
fn history_points(samples: &[&Sample], now: i64) -> Vec<(i64, Option<f64>)> {
    let mut points: Vec<_> = samples
        .iter()
        .filter_map(|s| {
            let at = timestamp(&s.at)?;
            (at >= now - 86_400 && at <= now)
                .then_some((at, s.used.filter(|v| s.readable && v.is_finite() && (0.0..=100.0).contains(v))))
        })
        .collect();
    points.sort_by_key(|p| p.0);
    points
}
fn history_label(points: &[(i64, Option<f64>)]) -> &'static str {
    if points.iter().all(|(_, v)| v.is_none()) {
        "History not measured"
    } else if points.iter().any(|(_, v)| v.is_none()) {
        "History partially unreadable"
    } else {
        "Recorded samples in the last 24 hours"
    }
}
fn projection(points: &[(i64, Option<f64>)], reset: Option<&str>, now: i64) -> Option<String> {
    let reset = timestamp(reset?)?;
    if reset <= now {
        return None;
    }
    let &(last_at, Some(last)) = points.last()? else {
        return None;
    };
    if now - last_at > 1800 {
        return None;
    }
    let mut first = (last_at, last);
    for &(at, value) in points.iter().rev().skip(1) {
        let Some(value) = value else {
            break;
        };
        if value > first.1 || first.0 - at > 1800 {
            break;
        }
        first = (at, value);
    }
    let elapsed = last_at - first.0;
    let increase = last - first.1;
    if elapsed <= 0 || increase <= 0.0 {
        return None;
    }
    let predicted = last_at as f64 + (80.0 - last).max(0.0) / increase * elapsed as f64;
    if predicted >= reset as f64 {
        Some("Reset before projected throttle".into())
    } else {
        Some(format!("Estimated {:.1} hours to 80% throttle", ((predicted - now as f64) / 3600.0).max(0.0)))
    }
}
fn sparkline(points: &[(i64, Option<f64>)], now: i64) -> Markup {
    let mut segments = vec![String::new()];
    let mut previous = None;
    for &(at, value) in points {
        if let Some(value) = value {
            if previous.is_some_and(|(t, v)| at - t > 1800 || value < v) {
                segments.push(String::new());
            }
            use std::fmt::Write;
            let _ = write!(
                segments.last_mut().expect("one segment"),
                "{:.2},{:.2} ",
                (at - (now - 86_400)) as f64 / 864.0,
                100.0 - value
            );
            previous = Some((at, value));
        } else {
            segments.push(String::new());
            previous = None;
        }
    }
    html! { div class="mh-spark" role="img" aria-label="Recorded usage in the last 24 hours. Gaps, resets and missing history are not connected." {
        svg viewBox="0 0 100 100" preserveAspectRatio="none" aria-hidden="true" { @for segment in segments { polyline points=(segment) {} } }
    } }
}
#[derive(Debug)]
struct Event {
    at: i64,
    lane: String,
    state: String,
}
fn log_events(log: &Section<String>, now: i64) -> Section<Vec<Event>> {
    let Some(text) = &log.data else {
        return Section { source: log.source.clone(), data: None, errors: log.errors.clone() };
    };
    let mut events = Vec::new();
    let mut errors = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let mut words = line.split_whitespace();
        let Some(at) = words.next().and_then(timestamp) else {
            errors.push(SourceError {
                at: format!("{}:record {}", log.source, i + 1),
                reason: "Invalid incident timestamp".into(),
            });
            continue;
        };
        if at < now - 86_400 || at > now {
            continue;
        }
        let words: Vec<_> = words.collect();
        let mut parsed = false;
        for pair in words.windows(2) {
            if let Some((_, to)) = pair[1].trim_end_matches(';').split_once("->")
                && pair[0].contains('/')
                && !to.is_empty()
            {
                events.push(Event { at, lane: format!("Quota · {}", pair[0]), state: to.into() });
                parsed = true;
            }
        }
        if line.contains("->") && !parsed {
            errors.push(SourceError {
                at: format!("{}:record {}", log.source, i + 1),
                reason: "Unrecognized incident transition".into(),
            });
        }
    }
    Section::partial(log.source.clone(), events, errors)
}
fn intervals<'a>(rows: &[&'a Event], end: i64) -> Vec<(i64, i64, &'a str)> {
    let mut out: Vec<(i64, i64, &str)> = Vec::new();
    for (i, event) in rows.iter().enumerate() {
        let until = rows.get(i + 1).map_or(end.min(event.at + 1800), |next| next.at.min(event.at + 1800));
        let class = state(&event.state).0;
        if let Some(last) = out.last_mut().filter(|last| last.2 == class && last.1 >= event.at) {
            last.1 = until;
        } else {
            out.push((event.at, until, class));
        }
    }
    out
}
fn timeline(events: &[Event], end: i64, now: i64) -> Markup {
    let mut lanes = BTreeMap::<&str, Vec<&Event>>::new();
    for event in events {
        if event.at >= now - 86_400 && event.at <= end {
            lanes.entry(&event.lane).or_default().push(event);
        }
    }
    html! {
        p class="mh-note" { "Recorded transitions and census samples, in server local time. Blank intervals have no evidence; recorded states do not prove continuous uptime." }
        div class="mh-time-ticks" aria-hidden="true" { @for ago in [24,18,12,6,0] { span { (local(now-ago*3600)) } } }
        @if lanes.is_empty() { p { "No incident samples recorded in this window. This does not certify a healthy fleet." } }
        @if lanes.len()>24 { p class="mh-note" { "Showing 24 of " (lanes.len()) " observed lanes." } }
        @for (lane,mut rows) in lanes.into_iter().take(24) { @let spans={ rows.sort_by_key(|e| e.at); intervals(&rows,end) };
            div class="mh-incident" { strong { (safe_lane(lane)) }
                div class="mh-incident-track" aria-hidden="true" {
                    @for (at,until,class) in spans.iter().skip(spans.len().saturating_sub(96)) {
                        span class="mh-incident-segment" data-state=(class) style=(format!("left:{:.3}%;width:{:.3}%", (at-(now-86_400)) as f64/864.0, (until-at).max(0) as f64/864.0)) {}
                    }
                }
                details { summary { (rows.len()) " recorded samples" }
                    @if rows.len()>12 { p class="mh-note" { "Showing the latest 12. Full evidence remains in the source log." } }
                    @if spans.len()>96 { p class="mh-note" { "Chart shows the latest 96 intervals." } }
                    ul { @for event in rows.into_iter().rev().take(12) { li { (local(event.at)) " · " (chip(&event.state)) " · " (event.state.as_str()) } } }
                }
            }
        }
    }
}
fn errors(errors: &[SourceError]) -> Markup {
    html! { @if errors.len()>8 { p { (errors.len()) " read errors; showing the first 8." } }
        @for error in errors.iter().take(8) { p { (error.at) ": " (error.reason) } }
    }
}
fn evidence(value: &Value) -> Markup {
    let text = serde_json::to_string_pretty(value).unwrap_or_default();
    html! { pre { (text.chars().take(2048).collect::<String>()) }
        @if text.chars().count()>2048 { p class="mh-note" { "Evidence preview limited to 2048 characters. See the source for the complete record." } }
    }
}
fn safe_lane(s: &str) -> String {
    if let Some((prefix, account)) = s.split_once('/') {
        format!("{prefix} · {}", account_label(account))
    } else {
        s.into()
    }
}
fn signal_events(signals: &Section<Vec<Signal>>) -> Vec<Event> {
    signals
        .data
        .as_ref()
        .into_iter()
        .flatten()
        .filter_map(|s| {
            Some(Event {
                at: timestamp(&s.at)?,
                lane: format!("Provider · {} · {}", s.host, s.instance),
                state: s.to.clone(),
            })
        })
        .collect()
}
fn census_events(samples: &Section<Vec<CensusSample>>, now: i64) -> Vec<Event> {
    let mut events = Vec::new();
    for sample in samples.data.as_ref().into_iter().flatten() {
        if let Some(hosts) = sample.host_load.as_object() {
            for (host, load) in hosts {
                let observed = sample.read_at.get(host).and_then(Value::as_str).or(sample.observed_at.as_deref());
                let at = observed.and_then(timestamp).or_else(|| timestamp(&sample.at));
                if let Some(at) = at.filter(|at| *at >= now - 86_400 && *at <= now) {
                    let fresh =
                        observed.is_some() && timestamp(&sample.at).is_some_and(|read| read - at <= 600 && read >= at);
                    let status = if !sample.complete || !fresh {
                        "unknown"
                    } else if has_values(&sample.errors[host]) {
                        "down"
                    } else if number(load, "live").is_some() {
                        "up"
                    } else {
                        "unknown"
                    };
                    events.push(Event { at, lane: format!("Host · {host}"), state: status.into() });
                }
            }
        }
    }
    events
}
fn dispatch(e: &Value) -> Markup {
    let counts = number(e, "decisions_24h").zip(number(e, "denials_24h")).zip(number(e, "followed"));
    html! { div class="mh-dispatch" { h3 { "Dispatch gate · 24 hours" }
        @if let Some(((total,denied),followed)) = counts.filter(|((total,denied),followed)| *total>=*denied && *denied>=*followed && *followed>=0.0 && [*total,*denied,*followed].iter().all(|n| n.fract()==0.0)) {
            p { (format!("{:.0} allowed · {:.0} sent elsewhere · {:.0} denied",total-denied,followed,denied-followed)) }
        } @else { p { "Dispatch counts not measured" } }
    } }
}
fn source_row<T>(label: &str, s: &Section<T>, stamp: Option<&str>, now: i64, cadence: u64) -> Markup {
    let (fresh, age) = sampled(stamp, now, cadence);
    html! { div class="mh-source" { strong { (label) } span { @if s.data.is_none() { "Unavailable" } @else if s.is_degraded() { "Partially read" } @else { "Read" } " · " (age) }
        @if !fresh { span class="mh-warning" { "Current status cannot be certified" } }
        details { summary { "Source details" } code { (s.source) } (errors(&s.errors)) }
    } }
}

pub(crate) fn health(cx: &Ctx, id: &str) -> Markup {
    let now = cx.at.as_second();
    let census = cx.store.health_census();
    let capacity = cx.store.health_capacity();
    let loops = cx.store.health_loops();
    let history = cx.store.health_history();
    let log = cx.store.health_log();
    let signals = cx.store.health_signals();
    let samples = cx.store.health_samples();
    let parsed = log_events(&log, now);
    let census_fresh = sampled(census.data.as_ref().and_then(|c| c.observed_at.as_deref()), now, 600).0;
    let ttl = capacity.data.as_ref().and_then(|c| c.ttl_seconds).filter(|n| *n > 0 && *n <= 1800).unwrap_or(1800);
    let capacity_fresh = sampled(capacity.data.as_ref().and_then(|c| c.generated_at.as_deref()), now, ttl).0;
    let loops_fresh = sampled(loops.data.as_ref().and_then(|c| c.evaluated_at.as_deref()), now, 600).0;
    let host_problem = census
        .data
        .as_ref()
        .is_some_and(|c| hosts(c).iter().any(|h| problematic(host_state(c, h, census_fresh && host_fresh(c, h, now)))));
    let provider_problem = census.data.as_ref().is_some_and(|c| {
        !c.active_signals.is_empty()
            || c.connections
                .as_object()
                .into_iter()
                .flat_map(|m| m.values())
                .filter_map(Value::as_object)
                .flat_map(|m| m.values())
                .any(|v| problematic(word(v, "state")))
    });
    let token_problem = capacity.data.as_ref().is_some_and(|c| {
        c.lanes.iter().any(|l| problematic(word(l, "state")) || percent(l, "used").is_some_and(|n| n >= 80.0))
            || has_values(&c.errors)
            || c.host_holds.as_object().is_some_and(|holds| holds.keys().any(|host| active_hold(c, host)))
    });
    let loop_problem = loops
        .data
        .as_ref()
        .and_then(|l| l.loops.as_object())
        .is_some_and(|m| m.values().any(|v| problematic(word(v, "status"))));
    let incomplete = !census_fresh
        || !capacity_fresh
        || !loops_fresh
        || census.is_degraded()
        || capacity.is_degraded()
        || loops.is_degraded()
        || history.is_degraded()
        || parsed.is_degraded()
        || signals.is_degraded()
        || samples.is_degraded()
        || loops.data.as_ref().is_some_and(|l| {
            l.loops
                .as_object()
                .is_none_or(|m| m.is_empty() || m.values().any(|v| state(word(v, "status")).0 == "unknown"))
        })
        || census.data.as_ref().is_some_and(|c| {
            !c.coverage_gaps.is_empty()
                || hosts(c).is_empty()
                || !provider_scope_complete(c)
                || hosts(c).iter().any(|h| host_state(c, h, host_fresh(c, h, now)) == "unknown")
                || has_values(&c.errors)
                || has_values(&c.sensor_errors)
        })
        || capacity.data.as_ref().is_some_and(|c| {
            c.lanes.is_empty()
                || c.lanes.iter().any(|l| percent(l, "used").is_none() || state(word(l, "state")).0 == "unknown")
        });
    let mut events = parsed
        .data
        .as_ref()
        .map(|v| v.iter().map(|e| Event { at: e.at, lane: e.lane.clone(), state: e.state.clone() }).collect::<Vec<_>>())
        .unwrap_or_default();
    events.extend(signal_events(&signals));
    events.extend(census_events(&samples, now));
    html! { div class="mh" data-block=(id) data-type="health" {
        section class="mh-verdict" aria-label="Fleet verdict" {
            h1 { "Health" }
            p class="mh-verdict-title" { @if host_problem || provider_problem || token_problem || loop_problem { "The fleet needs attention." } @else if incomplete { "Fleet status is incomplete." } @else { "No problems reported in current readings." } }
            p { "Host reachability, provider availability and capacity for the next dispatch." }
            nav class="mh-pills" aria-label="Health sections" { a href="#mh-hosts" { "Hosts" } a href="#mh-providers" { "Providers" } a href="#mh-tokens" { "Capacity" } a href="#mh-loops" { "Loops" } a href="#mh-sources" { "Sources" } }
            details class="mh-attention-details" { summary { "Warnings and missing readings" }
            ul class="mh-attention" {
                @if host_problem { li { "Host reachability or resource warnings were reported." } }
                @if provider_problem { li { "Some provider connections are limited or unavailable." } }
                @if token_problem { li { "Capacity is limited by quota, host holds or source errors." } }
                @if loop_problem { li { "Some loops have warnings or breaches." } }
                @if incomplete { li { "Some readings are missing, stale or outside the observed scope. Review sources before dispatch." } }
                @if let Some(c)=&census.data { @if !c.coverage_gaps.is_empty() { li { (c.coverage_gaps.len()) " account and host coverage gaps are not measured." } } }
            } }
        }
        div class="mh-upper" {
            section id="mh-hosts" { h2 { "Hosts" }
                p class="mh-note" { "CPU is load per core; memory is used percentage. Unmeasured resources stay unknown." }
                @if let Some(c)=&census.data { div class="mh-hosts" { @for host in hosts(c) {
                    @let h=&c.host_load[&host];@let fresh=census_fresh && host_fresh(c,&host,now);
                    @let held=capacity_fresh && capacity.data.as_ref().is_some_and(|cap|active_hold(cap,&host));
                    article class="mh-host" { div class="mh-host-heading" { h3 { (host.as_str()) } (chip(if held && fresh { "warn" } else { host_state(c,&host,fresh) })) }
                        @if held { p class="mh-warning" { "Dispatch held: " (capacity.data.as_ref().map(|cap|word(&cap.host_holds[&host],"why")).unwrap_or("Reason not recorded")) } }
                        p { @if let Some((live,cap))=number(h,"live").zip(number(&c.host_caps,&host)) { (format!("{live:.0} of {cap:.0} sessions")) } @else { "Sessions not measured" } }
                        dl class="mh-metrics" { @for (label,value,suffix) in [("CPU load / core",number(h,"load_per_cpu").map(|n|n*100.0),"%"),("Memory used",percent(h,"mem_avail_pct").map(|n|100.0-n),"%"),("CPU cores",number(h,"cpus"),"")] { div { dt { (label) } dd { @if let Some(value)=value { (format!("{value:.0}{suffix}")) } @else { "Not measured" } } } } }
                        p class="mh-note" { (sampled(c.host_last_observed[&host].get("at").and_then(Value::as_str),now,600).1) }
                    }
                } } } @else { p class="mh-unavailable" { "Host information is unavailable." } }
            }
            section id="mh-providers" { h2 { "Providers by host" }
                @if let Some(c)=&census.data { div class="mh-providers" { @for host in hosts(c) {
                    article class="mh-provider-host" { h3 { (host.as_str()) }
                        @if let Some(connections)=c.connections[&host].as_object() { @if connections.is_empty() { p { "No provider connections measured." } }
                            @let fresh=census_fresh && host_fresh(c,&host,now);
                            @let limited:Vec<_>=connections.iter().filter(|(_,value)|!fresh || matches!(state(word(value,"state")).0,"warn"|"fail"|"unknown")).collect();
                            @let other:Vec<_>=connections.iter().filter(|(_,value)|fresh && matches!(state(word(value,"state")).0,"ok"|"idle")).collect();
                            @if limited.is_empty() { p class="mh-note" { "No impaired connections reported." } }
                            ul { @for (name,value) in limited { li { strong { (name) } (chip(if fresh { word(value,"state") } else { "unknown" })) } } }
                            @if !other.is_empty() { details { summary { (other.len()) " other connections" }
                                ul { @for (name,value) in other { li { strong { (name) } (chip(word(value,"state"))) } } }
                            } }
                        } @else { p { "Provider connections not measured." } }
                    }
                } } } @else { p class="mh-unavailable" { "Provider information is unavailable." } }
            }
        }
        section class="mh-timeline" { h2 { "Last 24 hours" }
            @if parsed.is_degraded() || signals.is_degraded() || samples.is_degraded() { p class="mh-warning" { "Incident history is incomplete. Missing or malformed inputs cannot certify an incident-free interval." } }
            (timeline(&events,now,now))
        }
        div class="mh-lower" {
            section id="mh-tokens" { h2 { "Token capacity" } p class="mh-note" { "Subscription usage against the 80% dispatch throttle. Concurrency is a fleet setpoint, not a subscription limit." }
                @if let Some(c)=&capacity.data { @if c.lanes.is_empty() { p { "No quota lanes measured." } }
                    @for (i,lane) in c.lanes.iter().enumerate() {
                        @let provider=word(lane,"provider");@let account=word(lane,"account");@let used=percent(lane,"used");
                        @let routing=c.routing.iter().find(|r|word(r,"provider")==provider && word(r,"account")==account);
                        @let lane_samples:Vec<_>=history.data.as_ref().into_iter().flatten().filter(|s|s.lane==format!("{provider}/{account}")).collect();@let points=history_points(&lane_samples,now);
                        article class="mh-token" { div class="mh-token-heading" { h3 { (provider) " · " (account_label(account)) } (chip(lane_state(lane,capacity_fresh))) }
                            div class="mh-usage" { label for=(format!("mh-usage-{i}")) { @if let Some(n)=used { (format!("{n:.0}% used")) @if !capacity_fresh { " · stale reading" } } @else { "Usage not measured" } }
                                @if let Some(n)=used { meter id=(format!("mh-usage-{i}")) min="0" max="100" low="60" high="80" optimum="0" value=(n) { (format!("{n:.0}%")) } }
                            }
                            p { @if let Some(r)=routing { @if let Some((live,max))=number(r,"live").zip(number(r,"max_concurrent")) { (format!("{live:.0} of {max:.0} slots")) } @else { "Slots not measured" } } @else { "No routing slots reported" }
                                " · " @if let Some(reset)=reset_time(lane).and_then(timestamp) { "Resets " time datetime=(reset_time(lane).unwrap_or_default()) { (local(reset)) } } @else { "Reset not measured" }
                            }
                            details class="mh-history" { summary { "Usage history and estimate" }
                                (sparkline(&points,now)) p { (history_label(&points)) ". Gaps and resets break the line." }
                                p { (if history.is_degraded() || !capacity_fresh { None } else { projection(&points,reset_time(lane),now) }.unwrap_or_else(||"Projection not measured".into())) }
                                @if !points.is_empty() { p class="mh-note" { (points.len()) " recorded samples. Estimates assume the most recent continuous usage rate; they are not a dispatch promise." } }
                            }
                        }
                    }
                } @else { p class="mh-unavailable" { "Token capacity is unavailable. Configure the fleet telemetry directory to read the quota producer." } }
            }
            section id="mh-loops" { h2 { "Loops" }
                @if let Some(l)=&loops.data { @if let Some(map)=l.loops.as_object() { @if map.is_empty() { p { "No loops measured." } }
                    div class="mh-loops" { @for (name,value) in map { article { div class="mh-host-heading" { h3 { (name.replace('_'," ")) } (chip(if loops_fresh { word(value,"status") } else { "unknown" })) } p { (word(value,"why")) }
                        details { summary { "Recorded evidence" } (evidence(value.get("evidence").unwrap_or(&Value::Null))) }
                    } } }
                } @else { p class="mh-unavailable" { "Loop status has an invalid shape." } }
                @if let Some(e)=l.loops.get("dispatch_gate").and_then(|v|v.get("evidence")) { (dispatch(e)) }
                } @else { p class="mh-unavailable" { "Loop information is unavailable." } }
            }
        }
        section id="mh-sources" class="mh-sources" { h2 { "Scope and freshness" } p { "Read at " time datetime=(cx.at.to_string()) { (local(now)) } ". File modification times do not establish freshness. History is bounded to the last 1 MiB of each log." }
            (source_row("Fleet census",&census,census.data.as_ref().and_then(|c|c.observed_at.as_deref()),now,600))
            (source_row("Capacity",&capacity,capacity.data.as_ref().and_then(|c|c.generated_at.as_deref()),now,ttl))
            (source_row("Loops",&loops,loops.data.as_ref().and_then(|c|c.evaluated_at.as_deref()),now,600))
            details { summary { "History read details" }
                @for (label,source,read_errors) in [("Quota history",&history.source,&history.errors),("Quota incidents",&parsed.source,&parsed.errors),("Provider incidents",&signals.source,&signals.errors),("Host samples",&samples.source,&samples.errors)] {
                    h3 { (label) } code { (source) } (errors(read_errors))
                }
            }
        }
    } }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn dispatch_counts_require_consistent_evidence() {
        let good = dispatch(&json!({"decisions_24h":10,"denials_24h":3,"followed":2})).into_string();
        assert!(good.contains("7 allowed"));
        assert!(good.contains("2 sent elsewhere"));
        assert!(good.contains("1 denied"));
        for bad in [
            json!({"decisions_24h":2,"denials_24h":3,"followed":1}),
            json!({"decisions_24h":2.5,"denials_24h":1,"followed":0}),
        ] {
            assert!(dispatch(&bad).into_string().contains("not measured"));
        }
    }
    #[test]
    fn producer_time_decides_freshness() {
        let now = timestamp("2026-11-23T12:00:00Z").unwrap();
        assert!(sampled(Some("2026-11-23T11:50:00Z"), now, 600).0);
        assert!(!sampled(Some("2026-11-23T11:49:59Z"), now, 600).0);
        assert!(!sampled(Some("2026-11-23T12:02:00Z"), now, 600).0);
        assert!(!sampled(None, now, 600).0);
    }
    #[test]
    fn projection_requires_contiguous_readable_samples() {
        let now = 1_700_000_000;
        let reset = jiff::Timestamp::from_second(now + 20_000).unwrap().to_string();
        assert!(
            projection(&[(now - 1800, Some(20.0)), (now, Some(40.0))], Some(&reset), now).unwrap().contains("hours")
        );
        assert!(projection(&[(now - 3600, Some(20.0)), (now, Some(40.0))], Some(&reset), now).is_none());
        assert!(projection(&[(now - 1800, Some(90.0)), (now, Some(40.0))], Some(&reset), now).is_none());
        assert!(
            projection(&[(now - 1800, Some(20.0)), (now - 10, None), (now, Some(40.0))], Some(&reset), now).is_none()
        );
        assert!(projection(&[(now - 3600, Some(20.0)), (now - 1801, Some(40.0))], Some(&reset), now).is_none());
        assert!(projection(&[(now - 1800, Some(20.0)), (now, Some(40.0))], None, now).is_none());
        assert!(
            projection(&[(now - 1800, Some(90.0)), (now - 900, Some(10.0)), (now, Some(20.0))], Some(&reset), now)
                .is_some(),
            "a new continuous window after reset can project without using the old counter"
        );
        let soon = jiff::Timestamp::from_second(now + 100).unwrap().to_string();
        assert!(
            projection(&[(now - 1800, Some(20.0)), (now, Some(40.0))], Some(&soon), now)
                .unwrap()
                .contains("Reset before")
        );
    }
    #[test]
    fn malformed_incidents_remain_partial() {
        let now = timestamp("2026-11-23T12:00:00Z").unwrap();
        let events = log_events(
            &Section::ok("fixture", "invalid\n2026-11-23T11:00:00Z codex/team@example.com green->red".into()),
            now,
        );
        assert!(events.is_degraded());
        assert_eq!(events.data.unwrap().len(), 1);
    }
    #[test]
    fn gaps_and_resets_are_not_connected() {
        let points = [(100, Some(90.0)), (200, Some(20.0)), (300, None), (400, Some(30.0)), (3000, Some(40.0))];
        let s = sparkline(&points, 3000).into_string();
        assert_eq!(s.matches("<polyline").count(), 4);
        assert_eq!(history_label(&[(1, None)]), "History not measured");
        assert_eq!(history_label(&[(1, Some(1.0)), (2, None)]), "History partially unreadable");
    }
    #[test]
    fn host_readability_does_not_imply_reachability() {
        let c: Census=serde_json::from_value(json!({"host_load":{"mac":{"live":0,"load_per_cpu":0.1,"mem_avail_pct":50}},"host_caps":{"mac":8},"connections":{},"reachable_hosts":[]})).unwrap();
        assert_eq!(host_state(&c, "mac", true), "down");
        assert_eq!(host_state(&c, "mac", false), "unknown");
        assert_eq!(percent(&json!({"used":101}), "used"), None);
    }
    #[test]
    fn history_previews_are_bounded_and_gaps_stay_visible() {
        let events: Vec<_> =
            (0..5000).map(|i| Event { at: i, lane: "Host · mac".into(), state: "up".into() }).collect();
        let rendered = timeline(&events, 5000, 5000).into_string();
        assert!(rendered.len() < 10000, "{}", rendered.len());
        assert!(rendered.contains("latest 12"));
        let a = Event { at: 0, lane: "a".into(), state: "up".into() };
        let b = Event { at: 4000, lane: "a".into(), state: "up".into() };
        assert_eq!(intervals(&[&a, &b], 5000).len(), 2, "unobserved gap is not merged");
        let errs = vec![SourceError { at: "sample".into(), reason: "unread".into() }; 100];
        let e = errors(&errs).into_string();
        assert!(e.contains("100 read errors"));
        assert_eq!(e.matches("sample").count(), 8);
        let e = evidence(&json!({"long":"x".repeat(5000)})).into_string();
        assert!(e.contains("limited to 2048"));
        assert!(e.len() < 2400);
    }
    #[test]
    fn inactive_holds_and_green_quota_labels_do_not_hide_the_record() {
        let c: Capacity = serde_json::from_value(
            json!({"host":{},"lanes":[],"routing":[],"host_holds":{"mac":{"active":false},"vps":{"active":true}}}),
        )
        .unwrap();
        assert!(!active_hold(&c, "mac"));
        assert!(active_hold(&c, "vps"));
        assert_eq!(lane_state(&json!({"state":"green","used":95}), true), "red");
        assert_eq!(lane_state(&json!({"state":"green","used":null}), true), "unknown");
        assert_eq!(lane_state(&json!({"state":"green","used":40}), false), "unknown");
    }
}
