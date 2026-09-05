//! Read-only application discovery. This is not an attachment/authorization
//! mechanism and does not install a browser interaction provider.

#[cfg(any(target_os = "macos", test))]
mod probe;
#[cfg(test)]
mod tests;

use serde::Serialize;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;
#[cfg(any(target_os = "macos", test))]
use std::time::Instant;

pub const TOTAL_BUDGET: Duration = Duration::from_secs(8);
#[cfg(any(target_os = "macos", test))]
pub(crate) const MAX_APPS: usize = 16;
#[cfg(target_os = "macos")]
pub(crate) const MAX_PROCESSES: usize = 32;
#[cfg(any(target_os = "macos", test))]
pub(crate) const MAX_ENDPOINTS: usize = 8;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct ProcessIdentity {
    pub pid: i32,
    pub started_seconds: u64,
    pub started_micros: u64,
    pub executable: PathBuf,
}

#[derive(Debug, Clone)]
#[cfg(any(target_os = "macos", test))]
pub(crate) struct RunningApp {
    pub name: String,
    pub bundle_id: Option<String>,
    pub bundle: PathBuf,
    pub pid: i32,
    pub runtime: &'static str,
    pub evidence: Vec<String>,
    pub issues: Vec<String>,
}

#[derive(Debug, Clone)]
#[cfg(any(target_os = "macos", test))]
pub(crate) struct Candidate {
    pub address: SocketAddr,
    pub owner: ProcessIdentity,
    pub provenance: Vec<String>,
    pub expected_path: Option<String>,
}

#[derive(Debug, Default, Clone)]
#[cfg(any(target_os = "macos", test))]
pub(crate) struct Investigation {
    pub processes: Vec<ProcessIdentity>,
    pub candidates: Vec<Candidate>,
    pub issues: Vec<String>,
    pub stale_evidence: bool,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct Endpoint {
    pub address: SocketAddr,
    pub owner: ProcessIdentity,
    pub provenance: Vec<String>,
    pub status: &'static str,
    pub protocol_version: Option<String>,
    pub product: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Inspection {
    pub status: &'static str,
    pub apps: Vec<AppReport>,
    pub next_step: String,
}

#[derive(Debug, Serialize)]
pub struct AppReport {
    pub app: String,
    pub bundle_id: Option<String>,
    pub runtime: &'static str,
    pub status: &'static str,
    pub inspection: &'static str,
    pub native_route: &'static str,
    pub next_step: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Details>,
}

#[derive(Debug, Serialize)]
pub struct Details {
    runtime_confidence: &'static str,
    browser_tools: &'static str,
    enablement: &'static str,
    bundle_path: PathBuf,
    runtime_evidence: Vec<String>,
    processes: Vec<ProcessIdentity>,
    endpoints: Vec<Endpoint>,
    issues: Vec<String>,
    compatibility: &'static str,
}

#[derive(Clone, Copy, PartialEq, Eq)]
#[cfg(any(target_os = "macos", test))]
pub(crate) enum Ownership {
    Current,
    Stale,
    Unavailable,
}

#[cfg(any(target_os = "macos", test))]
pub(crate) trait Source {
    fn apps(&self, query: Option<&str>, deadline: Instant) -> (Vec<RunningApp>, bool);
    fn native_available(&self) -> bool;
    fn investigate(&self, app: &RunningApp, deadline: Instant) -> Investigation;
    /// Recheck both process start identity and ownership of this exact socket.
    fn ownership(&self, candidate: &Candidate, deadline: Instant) -> Ownership;
}

/// Public discovery entry. A single bounded investigation runs at a time;
/// cancellation does not release the slot until its blocking work has stopped.
pub fn inspect(app: Option<&str>, details: bool) -> Inspection {
    static ACTIVE: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let Ok(_active) = ACTIVE.try_lock() else {
        return empty("busy", "Application inspection is running; retry shortly.");
    };
    #[cfg(target_os = "macos")]
    {
        inspect_with(
            &crate::platform::mac::app_inspection::MacSource,
            app,
            details,
        )
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, details);
        empty(
            "unsupported",
            "Application discovery currently supports macOS. Existing native tools are unchanged.",
        )
    }
}

fn empty(status: &'static str, next: &str) -> Inspection {
    Inspection {
        status,
        apps: Vec::new(),
        next_step: next.to_string(),
    }
}

#[cfg(any(target_os = "macos", test))]
pub(crate) fn inspect_with(source: &impl Source, query: Option<&str>, details: bool) -> Inspection {
    let deadline = Instant::now() + TOTAL_BUDGET;
    let query = query
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_lowercase);
    let (mut apps, mut incomplete) = source.apps(query.as_deref(), deadline);
    // A nested Helper.app belongs to its outer running application. Bundle
    // ancestry, not similar display names, determines this deduplication.
    let roots: Vec<_> = apps.iter().map(|a| a.bundle.clone()).collect();
    apps.retain(|a| {
        !roots
            .iter()
            .any(|root| root != &a.bundle && a.bundle.starts_with(root))
    });
    apps.sort_by(|a, b| (&a.name, &a.bundle, a.pid).cmp(&(&b.name, &b.bundle, b.pid)));
    apps.dedup_by(|a, b| a.bundle == b.bundle && a.pid == b.pid);
    if let Some(query) = &query {
        let exact = apps.iter().any(|a| {
            a.name.to_lowercase() == *query
                || a.bundle_id
                    .as_deref()
                    .is_some_and(|id| id.eq_ignore_ascii_case(query))
        });
        apps.retain(|a| {
            let name = a.name.to_lowercase();
            let id = a.bundle_id.as_deref().unwrap_or_default().to_lowercase();
            if exact {
                name == *query || id == *query
            } else {
                name.contains(query) || id.contains(query)
            }
        });
    } else {
        apps.retain(|a| !a.evidence.is_empty() || !a.issues.is_empty());
    }
    if apps.len() > MAX_APPS {
        incomplete = true;
        apps.truncate(MAX_APPS);
    }
    let native = source.native_available();
    let mut reports = Vec::new();
    for app in apps {
        let mut investigation = if Instant::now() < deadline {
            source.investigate(&app, deadline)
        } else {
            Investigation {
                issues: vec!["total_deadline".into()],
                ..Default::default()
            }
        };
        investigation.issues.extend(app.issues);
        let mut endpoints = Vec::new();
        if investigation.candidates.len() > MAX_ENDPOINTS {
            investigation.issues.push("endpoint_limit".into());
        }
        for candidate in investigation.candidates.into_iter().take(MAX_ENDPOINTS) {
            if Instant::now() >= deadline {
                investigation.issues.push("total_deadline".into());
                break;
            }
            let mut endpoint = Endpoint {
                address: candidate.address,
                owner: candidate.owner.clone(),
                provenance: candidate.provenance.clone(),
                status: "stale_evidence",
                protocol_version: None,
                product: None,
            };
            let before = source.ownership(&candidate, deadline);
            if before == Ownership::Current {
                let result = probe::verify(&candidate, deadline);
                if result.status == "timed_out" {
                    investigation.issues.push("probe_deadline".into());
                }
                endpoint.status = result.status;
                endpoint.protocol_version = result.protocol_version;
                endpoint.product = result.product;
                let after = source.ownership(&candidate, deadline);
                if after != Ownership::Current {
                    endpoint.status = if after == Ownership::Stale {
                        "stale_evidence"
                    } else {
                        "ownership_unverified"
                    };
                    endpoint.protocol_version = None;
                    endpoint.product = None;
                }
            }
            if before == Ownership::Unavailable {
                endpoint.status = "ownership_unverified";
            }
            if endpoint.status == "ownership_unverified" {
                investigation.issues.push("owner_recheck_incomplete".into());
            }
            endpoints.push(endpoint);
        }
        let partial = !investigation.issues.is_empty();
        incomplete |= partial;
        let status = if endpoints
            .iter()
            .any(|e| e.status == "browser_handshake_verified")
        {
            "browser_endpoint_available"
        } else if endpoints.iter().any(|e| e.status == "node_inspector_only") {
            "node_inspector_only"
        } else if investigation.stale_evidence
            || endpoints.iter().any(|e| e.status == "stale_evidence")
        {
            "stale_endpoint_evidence"
        } else if endpoints
            .iter()
            .any(|e| e.status == "incompatible_endpoint")
        {
            "incompatible_endpoint"
        } else if !endpoints.is_empty() {
            "endpoint_unverified"
        } else {
            "no_endpoint_discovered"
        };
        let route = if native {
            "ax_read"
        } else {
            "accessibility_permission_required"
        };
        let native_next = if native {
            "Use ax_read for native interaction."
        } else {
            "Grant Nova Accessibility permission before using ax_read."
        };
        let next = if status == "browser_endpoint_available" {
            format!("Browser connection found, but browser tools are not attached or authorized by this inspection. {native_next}")
        } else {
            format!("{native_next} Browser enablement is unknown; inspect the app's own settings or a verified app-specific guide.")
        };
        reports.push(AppReport {
            app: app.name, bundle_id: app.bundle_id, runtime: app.runtime,
            status, inspection: if partial { "incomplete" } else { "complete" },
            native_route: route, next_step: next,
            details: details.then_some(Details {
                runtime_confidence: if app.evidence.is_empty() { "unknown" } else { "bundle_evidence" },
                browser_tools: "not_attached_compatibility_unverified", enablement: "unknown",
                bundle_path: app.bundle, runtime_evidence: app.evidence,
                processes: investigation.processes, endpoints, issues: investigation.issues,
                compatibility: "Browser handshake does not verify snapshot, fill, click, or the full Chrome DevTools MCP toolset.",
            }),
        });
    }
    Inspection {
        status: if incomplete { "incomplete" } else if reports.is_empty() { "no_match" } else { "complete" },
        apps: reports,
        next_step: if incomplete { "Some evidence is unavailable or limited. Follow each application's next step; optional details explain the inspection limits." } else { "Use an application's suggested native route. Inspection does not grant browser control." }.into(),
    }
}
