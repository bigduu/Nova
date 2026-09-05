//! NSWorkspace + bundle evidence, independent of ScreenCaptureKit and AX tree
//! reads. Process/port investigation stays inside the selected app's bundle.

mod process;
use crate::app_inspection::{
    Candidate, Investigation, Ownership, RunningApp, Source, MAX_PROCESSES,
};
use objc2_app_kit::NSWorkspace;
use std::collections::{HashSet, VecDeque};
use std::io::{self, Read};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{Instant, UNIX_EPOCH};

pub(crate) struct MacSource;

impl Source for MacSource {
    fn apps(&self, query: Option<&str>, deadline: Instant) -> (Vec<RunningApp>, bool) {
        let applications = NSWorkspace::sharedWorkspace().runningApplications();
        let mut out = Vec::new();
        let mut incomplete = applications.len() > 256;
        for app in applications.iter().take(256) {
            if Instant::now() >= deadline {
                incomplete = true;
                break;
            }
            if app.processIdentifier() <= 0 {
                continue;
            }
            let Some(path) = app.bundleURL().and_then(|url| url.path()) else {
                continue;
            };
            let path = PathBuf::from(path.to_string());
            out.push(RunningApp {
                name: app
                    .localizedName()
                    .map(|s| s.to_string())
                    .unwrap_or_default(),
                bundle_id: app.bundleIdentifier().map(|s| s.to_string()),
                bundle: path,
                pid: app.processIdentifier(),
                runtime: "unknown",
                evidence: Vec::new(),
                issues: Vec::new(),
            });
        }
        // NSWorkspace identity enumeration is cheap and independent of TCC;
        // explicit queries must not read unrelated apps' files or processes.
        if let Some(query) = query {
            let exact = out.iter().any(|a| {
                a.name.eq_ignore_ascii_case(query)
                    || a.bundle_id
                        .as_deref()
                        .is_some_and(|id| id.eq_ignore_ascii_case(query))
            });
            out.retain(|a| {
                let name = a.name.to_lowercase();
                let id = a.bundle_id.as_deref().unwrap_or_default().to_lowercase();
                if exact {
                    name == query || id == query
                } else {
                    name.contains(query) || id.contains(query)
                }
            });
        }
        for app in &mut out {
            if Instant::now() >= deadline {
                app.issues.push("total_deadline".into());
                incomplete = true;
                continue;
            }
            app.bundle = app
                .bundle
                .canonicalize()
                .unwrap_or_else(|_| app.bundle.clone());
            (app.runtime, app.evidence, app.issues) = runtime_evidence(&app.bundle, deadline);
        }
        (out, incomplete)
    }

    fn native_available(&self) -> bool {
        // SAFETY: passive check only. Never calls AXIsProcessTrustedWithOptions.
        unsafe { accessibility_sys::AXIsProcessTrusted() }
    }

    fn investigate(&self, app: &RunningApp, deadline: Instant) -> Investigation {
        let mut out = Investigation::default();
        let mut queue = VecDeque::from([(app.pid, 0)]);
        let mut seen = HashSet::new();
        while let Some((pid, depth)) = queue.pop_front() {
            if Instant::now() >= deadline {
                out.issues.push("total_deadline".into());
                break;
            }
            if !seen.insert(pid) {
                continue;
            }
            if out.processes.len() >= MAX_PROCESSES {
                out.issues.push("process_limit".into());
                break;
            }
            match process::identity(pid) {
                Ok(identity) if identity.executable.starts_with(&app.bundle) => {
                    out.processes.push(identity);
                    let (children, incomplete) = process::children(pid);
                    if incomplete {
                        out.issues.push("helper_enumeration_incomplete".into());
                    }
                    if depth < 4 {
                        queue.extend(children.into_iter().map(|pid| (pid, depth + 1)));
                    } else if !children.is_empty() {
                        out.issues.push("helper_depth_limit".into());
                    }
                }
                Ok(_) => {
                    if pid == app.pid {
                        out.issues.push("bundle_process_mismatch".into());
                    }
                }
                Err(error) => out.issues.push(
                    if error.kind() == io::ErrorKind::PermissionDenied {
                        "process_inspection_denied"
                    } else {
                        "process_identity_unavailable"
                    }
                    .into(),
                ),
            }
        }
        let pids = out.processes.iter().map(|p| p.pid).collect::<Vec<_>>();
        let listeners = match process::listeners(&pids, deadline) {
            Ok(listeners) => listeners,
            Err(_) => {
                out.issues.push("listener_inspection_incomplete".into());
                Vec::new()
            }
        };
        for (pid, address) in listeners {
            if let Some(owner) = out.processes.iter().find(|p| p.pid == pid) {
                out.candidates.push(Candidate {
                    address,
                    owner: owner.clone(),
                    provenance: vec!["process_owned_listener".into()],
                    expected_path: None,
                });
            }
        }
        let mut profiles = HashSet::new();
        let app_started = out
            .processes
            .iter()
            .find(|p| p.pid == app.pid)
            .map(|p| p.started_seconds)
            .unwrap_or(0);
        for identity in &out.processes {
            if Instant::now() >= deadline {
                out.issues.push("total_deadline".into());
                break;
            }
            let flags = match process::flags(identity.pid) {
                Ok(flags) => flags,
                Err(_) => {
                    out.issues.push("debug_flags_unavailable".into());
                    continue;
                }
            };
            if let Some(port) = flags.port.filter(|port| *port != 0) {
                if let Some(candidate) =
                    out.candidates.iter_mut().find(|c| c.address.port() == port)
                {
                    candidate
                        .provenance
                        .push("remote_debugging_port_flag".into());
                } else {
                    out.stale_evidence = true;
                    out.issues
                        .push("debug_port_has_no_verified_owned_listener".into());
                }
            }
            if let Some(profile) = flags.profile {
                let profile = match profile.canonicalize() {
                    Ok(profile) => profile,
                    Err(_) => {
                        out.issues.push("profile_location_unavailable".into());
                        continue;
                    }
                };
                if !profiles.insert(profile.clone()) {
                    continue;
                }
                if profiles.len() > 2 {
                    out.issues.push("profile_limit".into());
                    break;
                }
                // The browser writes this file before many helpers start.
                // Freshness belongs to the app root, not the helper argv that
                // happened to reveal its profile. Socket ownership is separate.
                match active_port(&profile, app_started) {
                    Ok(Some((port, path))) => {
                        if let Some(candidate) =
                            out.candidates.iter_mut().find(|c| c.address.port() == port)
                        {
                            candidate
                                .provenance
                                .push("profile_flag_DevToolsActivePort".into());
                            candidate.expected_path = Some(path);
                        } else {
                            out.stale_evidence = true;
                            out.issues
                                .push("active_port_has_no_verified_owned_listener".into());
                        }
                    }
                    Ok(None) => {}
                    Err(error) => {
                        if error.kind() == io::ErrorKind::InvalidData {
                            out.stale_evidence = true;
                            out.issues.push("active_port_stale_or_malformed".into());
                        } else {
                            out.issues.push("active_port_unreadable".into());
                        }
                    }
                }
            }
        }
        for candidate in &mut out.candidates {
            candidate.provenance.sort();
            candidate.provenance.dedup();
        }
        out.issues.sort();
        out.issues.dedup();
        out
    }

    fn ownership(&self, candidate: &Candidate, deadline: Instant) -> Ownership {
        if Instant::now() >= deadline {
            return Ownership::Unavailable;
        }
        let identity_matches = || match process::identity(candidate.owner.pid) {
            Ok(id) if id == candidate.owner => Ownership::Current,
            Ok(_) => Ownership::Stale,
            Err(error)
                if error.raw_os_error() == Some(libc::ESRCH)
                    || error.kind() == io::ErrorKind::NotFound =>
            {
                Ownership::Stale
            }
            Err(_) => Ownership::Unavailable,
        };
        let before = identity_matches();
        if before != Ownership::Current {
            return before;
        }
        match process::listeners(&[candidate.owner.pid], deadline) {
            Ok(rows) if rows.contains(&(candidate.owner.pid, candidate.address)) => {
                identity_matches()
            }
            Ok(_) => Ownership::Stale,
            Err(_) => Ownership::Unavailable,
        }
    }
}

fn runtime_evidence(bundle: &Path, deadline: Instant) -> (&'static str, Vec<String>, Vec<String>) {
    let frameworks = bundle.join("Contents/Frameworks");
    let entries = match std::fs::read_dir(&frameworks) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return ("unknown", Vec::new(), Vec::new())
        }
        Err(_) => {
            return (
                "unknown",
                Vec::new(),
                vec!["bundle_inspection_unavailable".into()],
            )
        }
    };
    let mut evidence = Vec::new();
    let mut issues = Vec::new();
    let mut runtime = "unknown";
    for (index, entry) in entries.enumerate() {
        if Instant::now() >= deadline {
            issues.push("total_deadline".into());
            break;
        }
        if index >= 64 {
            issues.push("framework_limit".into());
            break;
        }
        let Ok(entry) = entry else {
            issues.push("framework_unreadable".into());
            continue;
        };
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let kind = match name {
            "Electron Framework.framework" => "electron",
            "Chromium Embedded Framework.framework" => "cef",
            "Chromium Framework.framework"
            | "Google Chrome Framework.framework"
            | "Microsoft Edge Framework.framework"
            | "Brave Browser Framework.framework" => "chromium",
            _ => continue,
        };
        // Require the framework's executable as well as its container name.
        // Confidence is bundle evidence, not a claim of a loaded runtime/version.
        let executable_name = name.trim_end_matches(".framework");
        let mut paths = vec![entry.path().join(executable_name)];
        // Chrome distributions can retain versioned executables without a
        // top-level symlink. Only inspect this known framework's Versions dir.
        if let Ok(versions) = std::fs::read_dir(entry.path().join("Versions")) {
            for (index, version) in versions.enumerate() {
                if index >= 4 {
                    issues.push("framework_version_limit".into());
                    break;
                }
                if let Ok(version) = version {
                    paths.push(version.path().join(executable_name));
                }
            }
        }
        if let Some(path) = paths
            .into_iter()
            .filter_map(|p| p.canonicalize().ok())
            .find(|p| p.starts_with(bundle) && p.is_file())
        {
            runtime = kind;
            evidence.push(path.strip_prefix(bundle).unwrap().display().to_string());
        } else {
            issues.push("runtime_framework_unverified".into());
        }
    }
    (runtime, evidence, issues)
}

fn active_port(profile: &Path, started: u64) -> io::Result<Option<(u16, String)>> {
    if !profile.is_absolute() {
        return Err(io::Error::other("profile not absolute"));
    }
    let path = profile.join("DevToolsActivePort");
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let metadata = file.metadata()?;
    // SAFETY: geteuid has no preconditions. A stale file is evidence only;
    // live PID/start/listener checks are still required before and after probe.
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.len() > 2048
        || metadata
            .modified()?
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            < started
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "active port file is stale or untrusted",
        ));
    }
    let mut content = String::new();
    file.take(2049).read_to_string(&mut content)?;
    if content.len() > 2048 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "active port size limit",
        ));
    }
    let mut lines = content.lines();
    let port = lines
        .next()
        .and_then(|p| p.parse::<u16>().ok())
        .filter(|p| *p != 0);
    let path = lines.next().filter(|p| {
        p.starts_with("/devtools/browser/") && p.len() < 1024 && !p.contains(['?', '#', '\r', ' '])
    });
    match (port, path, lines.next()) {
        (Some(port), Some(path), None) => Ok(Some((port, path.to_string()))),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "active port malformed",
        )),
    }
}

#[cfg(test)]
mod tests;
